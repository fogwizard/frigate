#include "H/Function_Init.H"

/**************************************************************
说明：
1、Options for Target‘Target1’：BL51 Locate->Code Range:0x100，烧录选项请选择DISRST，复位脚作为普通IO使用；
2、串口1按 SSI1_Test 的 Uart1 方式初始化，9600 8N1，解析喂食器主机下发的 Modbus RTU；
3、两块板的从站地址用下面的宏区分，本板使用 MODBUS_SLAVE_ADDR。
***************************************************************/

/* 第一块喂食器从站 */
#define MODBUS_SLAVE_ADDR_1  0x01
/* 第二块喂食器从站 */
#define MODBUS_SLAVE_ADDR_2  0x02
/* 本板地址。第二块板改成 MODBUS_SLAVE_ADDR_2 */
#define MODBUS_SLAVE_ADDR    MODBUS_SLAVE_ADDR_1

#define MODBUS_FC_WRITE_COIL      0x05
#define MODBUS_FC_WRITE_REGISTER  0x06
#define MODBUS_FRAME_LEN          8
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

extern void Uart1_Init(uint Freq, unsigned long int baud);
extern bit Uart1SendFlag;
extern bit Uart1_Read(uint8_t *dat);

static uint8_t modbus_frame[MODBUS_FRAME_LEN];
static uint8_t modbus_len = 0;
/* 最近一次写到寄存器 0 的值。feed_open 非 0 表示打开喂食器。 */
volatile uint16_t feed_value = 0;
volatile uint8_t feed_open = 0;
static uint8_t tc118s_mode[2];

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

/* 功能码 0x06：写单个寄存器，value=1 打开，value=0 关闭。
   功能码 0x05：写单个线圈，0xFF00 打开，0x0000 关闭。
   地址匹配且 CRC 正确时回显 8 字节，主机据此确认写入。 */
static void modbus_on_frame(void)
{
	uint16_t crc;
	uint16_t address;
	uint16_t value;
	uint8_t function;

	crc = modbus_frame[6] | ((uint16_t)modbus_frame[7] << 8);
	if(crc != modbus_crc(modbus_frame, 6))
	{
		return;
	}
	if(modbus_frame[0] != MODBUS_SLAVE_ADDR)
	{
		modbus_len = 0;
		return;
	}
	function = modbus_frame[1];
	if(function != MODBUS_FC_WRITE_COIL && function != MODBUS_FC_WRITE_REGISTER)
	{
		modbus_len = 0;
		return;
	}

	address = ((uint16_t)modbus_frame[2] << 8) | modbus_frame[3];
	value = ((uint16_t)modbus_frame[4] << 8) | modbus_frame[5];
	if(address == FEED_REG_ADDRESS)
	{
		if(function == MODBUS_FC_WRITE_COIL)
		{
			feed_open = (value == 0xFF00) ? 1 : 0;
			feed_value = feed_open;
		}
		else
		{
			feed_value = value;
			feed_open = (value != 0) ? 1 : 0;
		}
		/* 打开喂食器正转，关闭停车（默认控制 CH0） */
		TC118S_Ctrl(TC118S_CH0, feed_open ? TC118S_FORWARD : TC118S_STOP);
	}
	uart1_send(modbus_frame, MODBUS_FRAME_LEN);
	modbus_len = 0;
}

static void modbus_push(uint8_t byte)
{
	uint8_t i;

	if(modbus_len < MODBUS_FRAME_LEN)
	{
		modbus_frame[modbus_len++] = byte;
	}
	else
	{
		for(i = 0; i < MODBUS_FRAME_LEN - 1; i++)
		{
			modbus_frame[i] = modbus_frame[i + 1];
		}
		modbus_frame[MODBUS_FRAME_LEN - 1] = byte;
	}
	if(modbus_len == MODBUS_FRAME_LEN)
	{
		modbus_on_frame();
	}
}

void main(void)
{
	uint8_t byte;

	IO_Init();
	TC118S_Init();
	WDTCON |= 0x10;
	Uart1_Init(16, 9600);
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
