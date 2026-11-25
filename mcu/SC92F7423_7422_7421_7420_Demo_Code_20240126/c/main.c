#include "H/Function_Init.H"

/**************************************************************
说明：
1、Options for Target‘Target1’：BL51 Locate->Code Range:0x100，烧录选项请选择DISRST，复位脚作为普通IO使用；
2、串口1按 SSI1_Test 的 Uart1 方式初始化，9600 8N1，解析喂食器主机下发的 Modbus RTU；
3、两块板的从站地址用下面的宏区分，本板使用 MODBUS_SLAVE_ADDR；
4、E22：M0=P0.6、M1=P0.7；Modbus 透传必须用模式0（M1=0 M0=0）。
   模式1(WOR)会改无线帧格式，MCU 侧常见只收到 7 字节且内容错乱。
***************************************************************/

/* 第一块喂食器从站 */
#define MODBUS_SLAVE_ADDR_1  0x01
/* 第二块喂食器从站 */
#define MODBUS_SLAVE_ADDR_2  0x02
/* 本板地址。第二块板改成 MODBUS_SLAVE_ADDR_2 */
#define MODBUS_SLAVE_ADDR    MODBUS_SLAVE_ADDR_1

#define MODBUS_FC_WRITE_COIL      0x05
#define MODBUS_FC_WRITE_REGISTER  0x06
#define MODBUS_FC_WRITE_MULTIPLE  0x10
#define MODBUS_EX_ILLEGAL_ADDRESS 0x02
#define MODBUS_EX_ILLEGAL_VALUE   0x03
#define MODBUS_RX_MAX             32
#define FEED_REG_ADDRESS          0x0000

/* TC118S 双路：CH0=P1.4/P1.5，CH1=P2.6/P2.7，P1.1=总电源使能 */
#define TC118S_EN                 P11
#define TC118S_CH0_INA            P14
#define TC118S_CH0_INB            P15
#define TC118S_CH1_INA            P26
#define TC118S_CH1_INB            P27
#define TC118S_CH0                0
#define TC118S_CH1                1
#define TC118S_STOP               0  /* INA=0 INB=0 stop */
#define TC118S_FORWARD            1  /* INA=1 INB=0 forward */
#define TC118S_REVERSE            2  /* INA=0 INB=1 reverse */
#define TC118S_BRAKE              3  /* INA=1 INB=1 brake */

/* E22 模式脚：M0=P0.6，M1=P0.7 */
#define E22_M0                    P06
#define E22_M1                    P07
#define E22_MODE_0_TRANSPARENT    0  /* M1=0 M0=0 */
#define E22_MODE_1_WOR            1  /* M1=0 M0=1 */
#define E22_MODE_2_CONFIG         2  /* M1=1 M0=0 */
#define E22_MODE_3_SLEEP          3  /* M1=1 M0=1 */

extern void Uart1_Init(uint Freq, unsigned long int baud);
extern bit Uart1SendFlag;
extern bit Uart1_Read(uint8_t *dat);

static uint8_t modbus_frame[MODBUS_RX_MAX];
static uint8_t modbus_len = 0;
/* 最近一次写到寄存器 0 的值。feed_open 非 0 表示打开喂食器。 */
volatile uint16_t feed_value = 0;
volatile uint8_t feed_open = 0;
static uint8_t tc118s_mode[2];

/**************************************************************
*函数名称：void E22_DelayMs(uint16_t ms)
*函数功能：粗延时，供模式脚切换后模块稳定
**************************************************************/
static void E22_DelayMs(uint16_t ms)
{
	uint16_t i;
	uint16_t j;

	for(i = 0; i < ms; i++)
	{
		for(j = 0; j < 900; j++)
		{
			;
		}
	}
}

/**************************************************************
*函数名称：void E22_SetMode(uint8_t mode)
*函数功能：设置 E22 工作模式（M1/M0）
*入口参数：mode=E22_MODE_0~3
**************************************************************/
static void E22_SetMode(uint8_t mode)
{
	switch(mode)
	{
		case E22_MODE_1_WOR:
			E22_M1 = 0;
			E22_M0 = 1;
			break;
		case E22_MODE_2_CONFIG:
			E22_M1 = 1;
			E22_M0 = 0;
			break;
		case E22_MODE_3_SLEEP:
			E22_M1 = 1;
			E22_M0 = 1;
			break;
		default:
			E22_M1 = 0;
			E22_M0 = 0;
			break;
	}
	/* 模式切换后模块需短暂稳定，手册一般要求约数毫秒 */
	E22_DelayMs(10);
}

/**************************************************************
*函数名称：void E22_Init(void)
*函数功能：P0.6/P0.7 推挽输出，上电进入模式0 透传（M1=0 M0=0）
**************************************************************/
static void E22_Init(void)
{
	P0CON |= 0xC0;   /* P0.6/P0.7 push-pull */
	P0PH  &= ~0xC0;
	/* 喂食 Modbus 走透明串口；WOR(模式1) 不能直接透传 8 字节 RTU */
	E22_SetMode(E22_MODE_0_TRANSPARENT);
}

/**************************************************************
*函数名称：void TC118S_Init(void)
*函数功能：双路 INA/INB 与 P1.1 使能脚推挽输出，上电停车
**************************************************************/
static void TC118S_Init(void)
{
	P1CON |= 0x32;   /* P1.1/P1.4/P1.5 push-pull */
	P1PH  &= ~0x32;
	P2CON |= 0xC0;   /* P2.6/P2.7 push-pull */
	P2PH  &= ~0xC0;
	TC118S_EN = 0;
	TC118S_CH0_INA = 0;
	TC118S_CH0_INB = 0;
	TC118S_CH1_INA = 0;
	TC118S_CH1_INB = 0;
	tc118s_mode[0] = TC118S_STOP;
	tc118s_mode[1] = TC118S_STOP;
}

/**************************************************************
*函数名称：void TC118S_Ctrl(uint8_t ch, uint8_t mode)
*函数功能：控制指定通道正转/反转/停车/刹车；任一路非 STOP 时打开 P1.1
*入口参数：ch=TC118S_CH0/CH1；mode=STOP/FORWARD/REVERSE/BRAKE
**************************************************************/
static void TC118S_Ctrl(uint8_t ch, uint8_t mode)
{
	if(ch > TC118S_CH1)
	{
		return;
	}

	if(ch == TC118S_CH0)
	{
		switch(mode)
		{
			case TC118S_FORWARD:
				TC118S_CH0_INA = 1;
				TC118S_CH0_INB = 0;
				break;
			case TC118S_REVERSE:
				TC118S_CH0_INA = 0;
				TC118S_CH0_INB = 1;
				break;
			case TC118S_BRAKE:
				TC118S_CH0_INA = 1;
				TC118S_CH0_INB = 1;
				break;
			default:
				TC118S_CH0_INA = 0;
				TC118S_CH0_INB = 0;
				mode = TC118S_STOP;
				break;
		}
	}
	else
	{
		switch(mode)
		{
			case TC118S_FORWARD:
				TC118S_CH1_INA = 1;
				TC118S_CH1_INB = 0;
				break;
			case TC118S_REVERSE:
				TC118S_CH1_INA = 0;
				TC118S_CH1_INB = 1;
				break;
			case TC118S_BRAKE:
				TC118S_CH1_INA = 1;
				TC118S_CH1_INB = 1;
				break;
			default:
				TC118S_CH1_INA = 0;
				TC118S_CH1_INB = 0;
				mode = TC118S_STOP;
				break;
		}
	}

	tc118s_mode[ch] = mode;
	TC118S_EN = (tc118s_mode[TC118S_CH0] != TC118S_STOP ||
	             tc118s_mode[TC118S_CH1] != TC118S_STOP) ? 1 : 0;
}

static uint16_t modbus_crc(const uint8_t *buf, uint8_t len)
{
	uint16_t crc = 0xFFFF;
	uint8_t i;
	uint8_t j;

	for(i = 0; i < len; i++)
	{
		crc ^= buf[i];
		for(j = 0; j < 8; j++)
		{
			if(crc & 0x0001)
			{
				crc = (crc >> 1) ^ 0xA001;
			}
			else
			{
				crc >>= 1;
			}
		}
	}
	return crc;
}

static void uart1_send(const uint8_t *buf, uint8_t len)
{
	uint8_t i;

	for(i = 0; i < len; i++)
	{
		SS1DAT = buf[i];
		while(!Uart1SendFlag);
		Uart1SendFlag = 0;
	}
}

/* 应答 PDU 后另算 CRC 再发出。pdu_len 不含 CRC，最长 6。 */
static void modbus_reply_pdu(const uint8_t *pdu, uint8_t pdu_len)
{
	uint8_t rsp[8];
	uint16_t crc;
	uint8_t i;

	if(pdu_len > 6)
	{
		return;
	}
	for(i = 0; i < pdu_len; i++)
	{
		rsp[i] = pdu[i];
	}
	crc = modbus_crc(rsp, pdu_len);
	rsp[pdu_len] = (uint8_t)(crc & 0xFF);
	rsp[pdu_len + 1] = (uint8_t)(crc >> 8);
	uart1_send(rsp, (uint8_t)(pdu_len + 2));
}

/* 异常应答：从站、功能码|0x80、异常码、CRC。共 5 字节。 */
static void modbus_reply_exception(uint8_t function, uint8_t exc)
{
	uint8_t pdu[3];

	pdu[0] = MODBUS_SLAVE_ADDR;
	pdu[1] = (uint8_t)(function | 0x80);
	pdu[2] = exc;
	modbus_reply_pdu(pdu, 3);
}

/* 写应答 8 字节：从站、功能码、地址、字段、CRC。
   0x05 的字段是线圈值 0xFF00/0x0000。
   0x06 的字段是写入的寄存器值。
   0x10 的字段是寄存器数量，请求里的寄存器内容不进入应答。 */
static void modbus_reply_write(uint8_t function, uint16_t address, uint16_t word)
{
	uint8_t pdu[6];

	pdu[0] = MODBUS_SLAVE_ADDR;
	pdu[1] = function;
	pdu[2] = (uint8_t)(address >> 8);
	pdu[3] = (uint8_t)(address & 0xFF);
	pdu[4] = (uint8_t)(word >> 8);
	pdu[5] = (uint8_t)(word & 0xFF);
	modbus_reply_pdu(pdu, 6);
}

static void feed_apply(uint16_t value, uint8_t as_coil)
{
	if(as_coil)
	{
		feed_open = (value == 0xFF00) ? 1 : 0;
		feed_value = feed_open;
	}
	else
	{
		feed_value = value;
		feed_open = (value != 0) ? 1 : 0;
	}
	TC118S_Ctrl(TC118S_CH0, feed_open ? TC118S_FORWARD : TC118S_STOP);
}

/* CRC 正确返回 1，调用方丢掉这一帧。CRC 不对返回 0，调用方滑动 1 字节重对齐。 */
static uint8_t modbus_on_frame(uint8_t len)
{
	uint16_t crc;
	uint16_t address;
	uint16_t field;
	uint16_t quantity;
	uint16_t value;
	uint8_t function;
	uint8_t nbytes;

	if(len < 4)
	{
		return 0;
	}
	crc = modbus_frame[len - 2] | ((uint16_t)modbus_frame[len - 1] << 8);
	if(crc != modbus_crc(modbus_frame, (uint8_t)(len - 2)))
	{
		return 0;
	}
	if(modbus_frame[0] != MODBUS_SLAVE_ADDR)
	{
		return 1;
	}

	function = modbus_frame[1];
	address = ((uint16_t)modbus_frame[2] << 8) | modbus_frame[3];
	if(function == MODBUS_FC_WRITE_COIL || function == MODBUS_FC_WRITE_REGISTER)
	{
		field = ((uint16_t)modbus_frame[4] << 8) | modbus_frame[5];
		if(address != FEED_REG_ADDRESS)
		{
			modbus_reply_exception(function, MODBUS_EX_ILLEGAL_ADDRESS);
			return 1;
		}
		if(function == MODBUS_FC_WRITE_COIL && field != 0xFF00 && field != 0x0000)
		{
			modbus_reply_exception(function, MODBUS_EX_ILLEGAL_VALUE);
			return 1;
		}
		feed_apply(field, (uint8_t)(function == MODBUS_FC_WRITE_COIL));
		modbus_reply_write(function, address, field);
		return 1;
	}

	quantity = ((uint16_t)modbus_frame[4] << 8) | modbus_frame[5];
	nbytes = modbus_frame[6];
	if(address != FEED_REG_ADDRESS || quantity != 1)
	{
		modbus_reply_exception(function, MODBUS_EX_ILLEGAL_ADDRESS);
		return 1;
	}
	if(nbytes != 2 || len != 11)
	{
		modbus_reply_exception(function, MODBUS_EX_ILLEGAL_VALUE);
		return 1;
	}
	value = ((uint16_t)modbus_frame[7] << 8) | modbus_frame[8];
	feed_apply(value, 0);
	modbus_reply_write(function, address, quantity);
	return 1;
}

/* 0：0x10 头部还没齐。8 或 9+字节数：一帧长度。0xFF：对齐失败，丢掉 1 字节。 */
static uint8_t modbus_expected_len(void)
{
	uint8_t n;

	switch(modbus_frame[1])
	{
		case MODBUS_FC_WRITE_COIL:
		case MODBUS_FC_WRITE_REGISTER:
			return 8;
		case MODBUS_FC_WRITE_MULTIPLE:
			if(modbus_len < 7)
			{
				return 0;
			}
			n = (uint8_t)(9 + modbus_frame[6]);
			if(modbus_frame[6] > 16)
			{
				return 0xFF;
			}
			return n;
		default:
			return 0xFF;
	}
}

static void modbus_slide(void)
{
	uint8_t i;

	if(modbus_len == 0)
	{
		return;
	}
	for(i = 1; i < modbus_len; i++)
	{
		modbus_frame[i - 1] = modbus_frame[i];
	}
	modbus_len--;
}

static void modbus_consume(uint8_t n)
{
	uint8_t i;

	if(n >= modbus_len)
	{
		modbus_len = 0;
		return;
	}
	for(i = n; i < modbus_len; i++)
	{
		modbus_frame[i - n] = modbus_frame[i];
	}
	modbus_len = (uint8_t)(modbus_len - n);
}

static void modbus_try(void)
{
	uint8_t need;

	while(modbus_len >= 2)
	{
		need = modbus_expected_len();
		if(need == 0)
		{
			return;
		}
		if(need == 0xFF)
		{
			modbus_slide();
			continue;
		}
		if(modbus_len < need)
		{
			return;
		}
		if(!modbus_on_frame(need))
		{
			modbus_slide();
			continue;
		}
		modbus_consume(need);
	}
}

static void modbus_push(uint8_t byte)
{
	if(modbus_len >= MODBUS_RX_MAX)
	{
		modbus_slide();
	}
	modbus_frame[modbus_len++] = byte;
	modbus_try();
}

void main(void)
{
	uint8_t byte;

	IO_Init();
	TC118S_Init();
	WDTCON |= 0x10;
	Uart1_Init(16, 9600);
	E22_Init();  /* 串口就绪后切到模式0透传：M1=0 M0=0 */
//0	TC118S_Ctrl(0, TC118S_FORWARD);
//	TC118S_Ctrl(1, TC118S_FORWARD);

	while(1)
	{
		WDTCON |= 0x10;
		if(Uart1_Read(&byte))
		{
			modbus_push(byte);
		}
	}
}
