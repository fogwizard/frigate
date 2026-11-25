export LIBVA_DRIVER_NAME=iHD
export LD_LIBRARY_PATH="/home/fog/.local/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
./target/release/vigil --config config/default.yaml --web-dir web
