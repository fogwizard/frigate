//! File-backed shared frame ring (Frigate-style): image bytes stay in mmap,
//! IPC only carries slot index + sequence.

use anyhow::{bail, Context, Result};
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

pub const MAGIC: u32 = 0x5647_5348; // "VGSH"
pub const VERSION: u32 = 1;
pub const FORMAT_JPEG: u32 = 1;
pub const FORMAT_RGB8: u32 = 2;
pub const CAM_ID_CAP: usize = 64;

const ST_EMPTY: u32 = 0;
const ST_WRITING: u32 = 1;
const ST_READY: u32 = 2;
const ST_READING: u32 = 3;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 4;
const OFF_SLOT_COUNT: usize = 8;
const OFF_SLOT_CAP: usize = 12;
const OFF_WRITE_CURSOR: usize = 16;

fn header_size() -> usize {
    64
}

fn slot_meta_size() -> usize {
    128
}

fn slot_stride(capacity: u32) -> usize {
    slot_meta_size() + capacity as usize
}

fn total_size(slots: u32, capacity: u32) -> usize {
    header_size() + slots as usize * slot_stride(capacity)
}

fn slot_base(slot: u32, capacity: u32) -> usize {
    header_size() + slot as usize * slot_stride(capacity)
}

// Slot meta layout (little-endian)
const SM_STATE: usize = 0;
const SM_SEQ: usize = 8;
const SM_WIDTH: usize = 16;
const SM_HEIGHT: usize = 20;
const SM_FORMAT: usize = 24;
const SM_DATA_LEN: usize = 28;
const SM_CAMERA: usize = 32; // 64 bytes

pub struct FrameRing {
    path: PathBuf,
    slot_count: u32,
    slot_capacity: u32,
    mmap: Mutex<MmapMut>,
}

#[derive(Debug, Clone)]
pub struct FrameView {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub data: Vec<u8>,
    pub seq: u64,
    pub slot: u32,
}

impl FrameRing {
    pub fn open(
        path: impl AsRef<Path>,
        slot_count: u32,
        slot_capacity: u32,
        create: bool,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let need = total_size(slot_count, slot_capacity) as u64;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .open(&path)
            .with_context(|| format!("open shm {}", path.display()))?;

        let meta_len = file.metadata()?.len();
        let recreate = create
            && (meta_len != need || !header_matches(&path, slot_count, slot_capacity)?);

        if recreate {
            file.set_len(0)?;
            file.set_len(need)?;
            let mut mmap = unsafe { MmapMut::map_mut(&file)? };
            mmap.fill(0);
            write_u32(&mut mmap, OFF_MAGIC, MAGIC);
            write_u32(&mut mmap, OFF_VERSION, VERSION);
            write_u32(&mut mmap, OFF_SLOT_COUNT, slot_count);
            write_u32(&mut mmap, OFF_SLOT_CAP, slot_capacity);
            write_u32(&mut mmap, OFF_WRITE_CURSOR, 0);
            mmap.flush()?;
            tracing::info!(
                path = %path.display(),
                slots = slot_count,
                capacity = slot_capacity,
                "created frame shm ring"
            );
            return Ok(Self {
                path,
                slot_count,
                slot_capacity,
                mmap: Mutex::new(mmap),
            });
        }

        if meta_len < need {
            bail!(
                "shm {} too small ({} < {}); start vigil first to create it",
                path.display(),
                meta_len,
                need
            );
        }

        let mmap = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            path,
            slot_count,
            slot_capacity,
            mmap: Mutex::new(mmap),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    pub fn slot_capacity(&self) -> u32 {
        self.slot_capacity
    }

    /// Claim a slot for direct fill (e.g. `read_exact` into [`SlotWrite::payload_mut`]).
    ///
    /// Payload pointer stays valid across `.await` without holding the mmap mutex:
    /// this slot is exclusively `WRITING` until [`SlotWrite::commit`] / drop.
    pub fn begin_write(
        &self,
        camera_id: &str,
        width: u32,
        height: u32,
        format: u32,
        data_len: usize,
    ) -> Result<SlotWrite<'_>> {
        if data_len > self.slot_capacity as usize {
            bail!(
                "frame {} bytes exceeds shm slot capacity {}",
                data_len,
                self.slot_capacity
            );
        }
        let mut mmap = self.mmap.lock().map_err(|_| anyhow::anyhow!("shm lock"))?;

        let cursor = unsafe {
            &*(mmap.as_ptr().add(OFF_WRITE_CURSOR) as *const AtomicU32)
        };
        let slot = cursor.fetch_add(1, Ordering::Relaxed) % self.slot_count;
        let base = slot_base(slot, self.slot_capacity);

        let state = unsafe { &*(mmap.as_ptr().add(base + SM_STATE) as *const AtomicU32) };
        for _ in 0..200 {
            let st = state.load(Ordering::Acquire);
            if st == ST_EMPTY || st == ST_READY {
                if state
                    .compare_exchange(st, ST_WRITING, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    break;
                }
            }
            std::thread::yield_now();
        }
        if state.load(Ordering::Acquire) != ST_WRITING {
            state.store(ST_WRITING, Ordering::Release);
        }

        let seq_atom = unsafe { &*(mmap.as_ptr().add(base + SM_SEQ) as *const AtomicU64) };
        let seq = seq_atom.load(Ordering::Relaxed).wrapping_add(1);
        seq_atom.store(seq, Ordering::Relaxed);

        write_u32(&mut mmap, base + SM_WIDTH, width);
        write_u32(&mut mmap, base + SM_HEIGHT, height);
        write_u32(&mut mmap, base + SM_FORMAT, format);
        write_u32(&mut mmap, base + SM_DATA_LEN, data_len as u32);

        {
            let cam = &mut mmap[base + SM_CAMERA..base + SM_CAMERA + CAM_ID_CAP];
            cam.fill(0);
            let idb = camera_id.as_bytes();
            let n = idb.len().min(CAM_ID_CAP);
            cam[..n].copy_from_slice(&idb[..n]);
        }

        let pay_off = base + slot_meta_size();
        // SAFETY: MmapMut is not remapped for the life of FrameRing; exclusive WRITING.
        let payload_ptr = unsafe { mmap.as_mut_ptr().add(pay_off) as usize };

        Ok(SlotWrite {
            ring: self,
            slot,
            seq,
            base,
            payload_ptr,
            data_len,
            committed: false,
        })
    }

    /// Publish frame bytes (copies into shm); returns (slot, seq) for IPC notify.
    pub fn publish(
        &self,
        camera_id: &str,
        width: u32,
        height: u32,
        format: u32,
        data: &[u8],
    ) -> Result<(u32, u64)> {
        let mut w = self.begin_write(camera_id, width, height, format, data.len())?;
        w.payload_mut().copy_from_slice(data);
        w.commit()
    }

    fn finish_write(&self, slot: u32, base: usize, ready: bool) -> Result<()> {
        let mmap = self.mmap.lock().map_err(|_| anyhow::anyhow!("shm lock"))?;
        let state = unsafe { &*(mmap.as_ptr().add(base + SM_STATE) as *const AtomicU32) };
        if ready {
            state.store(ST_READY, Ordering::Release);
            let _ = mmap.flush_range(base, slot_stride(self.slot_capacity));
        } else {
            state.store(ST_EMPTY, Ordering::Release);
        }
        let _ = slot;
        Ok(())
    }

    pub fn take(&self, slot: u32, expect_seq: u64) -> Result<FrameView> {
        if slot >= self.slot_count {
            bail!("slot {} out of range", slot);
        }
        let mmap = self.mmap.lock().map_err(|_| anyhow::anyhow!("shm lock"))?;
        let base = slot_base(slot, self.slot_capacity);

        let state = unsafe { &*(mmap.as_ptr().add(base + SM_STATE) as *const AtomicU32) };
        let st = state.load(Ordering::Acquire);
        if st != ST_READY {
            bail!("slot {slot} not ready (state={st})");
        }
        let seq_atom = unsafe { &*(mmap.as_ptr().add(base + SM_SEQ) as *const AtomicU64) };
        let seq = seq_atom.load(Ordering::Acquire);
        if seq != expect_seq {
            bail!("slot {slot} seq mismatch got={seq} expect={expect_seq}");
        }
        if state
            .compare_exchange(ST_READY, ST_READING, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            bail!("slot {slot} lost race");
        }

        let width = read_u32(&mmap, base + SM_WIDTH);
        let height = read_u32(&mmap, base + SM_HEIGHT);
        let format = read_u32(&mmap, base + SM_FORMAT);
        let len = read_u32(&mmap, base + SM_DATA_LEN) as usize;
        if len > self.slot_capacity as usize {
            state.store(ST_EMPTY, Ordering::Release);
            bail!("corrupt data_len {len}");
        }

        let cam_bytes = &mmap[base + SM_CAMERA..base + SM_CAMERA + CAM_ID_CAP];
        let cam_end = cam_bytes.iter().position(|&b| b == 0).unwrap_or(CAM_ID_CAP);
        let camera_id = String::from_utf8_lossy(&cam_bytes[..cam_end]).into_owned();

        let pay_off = base + slot_meta_size();
        let data = mmap[pay_off..pay_off + len].to_vec();

        // Leave READY so the ring acts as a multi-slot frame cache until the
        // writer wraps and overwrites this slot. Consumers validate via seq.
        state.store(ST_READY, Ordering::Release);
        Ok(FrameView {
            camera_id,
            width,
            height,
            format,
            data,
            seq,
            slot,
        })
    }

    /// Copy frame if slot+seq still match (for event snapshots after infer).
    pub fn peek(&self, slot: u32, expect_seq: u64) -> Result<FrameView> {
        self.take(slot, expect_seq)
    }
}

/// Exclusive write view into one ring slot. Fill [`Self::payload_mut`], then [`Self::commit`].
pub struct SlotWrite<'a> {
    ring: &'a FrameRing,
    slot: u32,
    seq: u64,
    base: usize,
    payload_ptr: usize,
    data_len: usize,
    committed: bool,
}

// Exclusive WRITING ownership of this slot; payload addr is stable while ring lives.
unsafe impl Send for SlotWrite<'_> {}

impl SlotWrite<'_> {
    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.payload_ptr as *mut u8, self.data_len) }
    }

    pub fn payload(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.payload_ptr as *const u8, self.data_len) }
    }

    pub fn commit(mut self) -> Result<(u32, u64)> {
        self.ring.finish_write(self.slot, self.base, true)?;
        self.committed = true;
        Ok((self.slot, self.seq))
    }
}

impl Drop for SlotWrite<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.ring.finish_write(self.slot, self.base, false);
        }
    }
}

fn write_u32(mmap: &mut MmapMut, off: usize, v: u32) {
    mmap[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn read_u32(mmap: &MmapMut, off: usize) -> u32 {
    u32::from_le_bytes(mmap[off..off + 4].try_into().unwrap())
}

fn header_matches(path: &Path, slots: u32, cap: u32) -> Result<bool> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = [0u8; 16];
    if f.read(&mut buf)? < 16 {
        return Ok(false);
    }
    Ok(u32::from_le_bytes(buf[0..4].try_into().unwrap()) == MAGIC
        && u32::from_le_bytes(buf[4..8].try_into().unwrap()) == VERSION
        && u32::from_le_bytes(buf[8..12].try_into().unwrap()) == slots
        && u32::from_le_bytes(buf[12..16].try_into().unwrap()) == cap)
}
