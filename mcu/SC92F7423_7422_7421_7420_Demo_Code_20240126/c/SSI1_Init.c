#include "H/Function_Init.H"

//选择三合一模式：
#define  Uart1  0
#define  TWI1    1
#define  SPI1    2

#define  SSI1_Mode  Uart1
#define SPI1_Mode 1 //0:主机工作模式   1：从机工作模式

void Uart1_Init(uint Freq,unsigned long int baud);
void TWI1_Init(void);
void SPI1_Init(void);

#define UART1_RX_SIZE 16

bit Uart1SendFlag = 0;     //Uart1发送中断标志位
bit Uart1ReceiveFlag = 0;  //Uart1接收中断标志位
volatile uint8_t Uart1_RxBuf[UART1_RX_SIZE];
volatile uint8_t Uart1_RxHead = 0;
volatile uint8_t Uart1_RxTail = 0;
bit SPI1Flag = 0;          //SPI1数据传输完成标志位
bit TWI1Flag = 0;          //TWI1中断标志位
uint8_t TxXferCount1 = 0;  //SPI发送数据数目
uint8_t RxXferCount1 = 0;  //SPI接受数据数目
uint8_t Uart1_RxData[5];
uint8_t SPI1_Master_SendData[8] = {0x01,0x02,0x03,0x04,0x05,0x06,0x07,0x08};
volatile uint8_t SPI1_Slaver_ReceiveData[8];

/*****************************************************
*函数名称：void SSI1_Test(void)
*函数功能：SSI1测试
*入口参数：void
*出口参数：void
*****************************************************/
void SSI1_Test(void)
{
#if (SSI1_Mode == Uart1)
	int num1,num2;
	Uart1_Init(16,9600);
	while(1)
	{
	for(num1=0;num1<5;num1++)
			{
				while(!Uart1ReceiveFlag);
				Uart1ReceiveFlag = 0;
				Uart1_RxData[num1] = SS1DAT ;
			}
			for(num2=0;num2<5;num2++)
			{
				SS1DAT  = Uart1_RxData[num2];
				while(!Uart1SendFlag);
				Uart1SendFlag = 0;
			}
	}
#endif
	
#if (SSI1_Mode == TWI1)
	TWI1_Init();
	while(1)
	{
	}
#endif

#if (SSI1_Mode == SPI1)
	SPI1_Init();
	while(1)
	{
#if (SPI1_Mode == 0)  //主机
				TxXferCount1 = 0;
				while(TxXferCount1 <  8)//判断是否接收所有数据
				{
				 
					SS1DAT = SPI1_Master_SendData[TxXferCount1];
					while(!SPI1Flag);
		      SPI1Flag = 0;	
					TxXferCount1 ++;	//发送数据量计数
				}
				{  int time = 1000;
            while(time--);
        }
#endif
#if (SPI1_Mode == 1)  //从机
			while(RxXferCount1 <8)
				{
					/* 等待SPI中断标志位置起 */
					while(!SPI1Flag);
	      	SPI1Flag = 0;	
					SPI1_Slaver_ReceiveData[RxXferCount1] = SS1DAT;			//读取数据
					RxXferCount1++;	//接收数据量计数加1       
				}
				RxXferCount1 = 0;
				{  int time = 100;
           while(time--);
        }

#endif
		}
#endif	
}

/*****************************************************
*函数名称：void Uart1_Init(uint Freq,unsigned long int baud)
*函数功能：Uart1中断初始化
*入口参数：Freq-主频，baud-波特率
*出口参数：void
*****************************************************/
void Uart1_Init(uint Freq,unsigned long int baud)
{
	unsigned long int reload;

	/* TX=P2.0 强推挽，RX=P2.1 输入上拉。
	   官方 demo 把 TX/RX 都设成输入上拉，接 E22 时驱动不足，易丢首字节/错码（只收到 7 字节）。 */
	P2CON |= 0x01;                       // P2.0 TX push-pull
	P2CON &= ~0x02;                      // P2.1 RX high-Z input
	P2PH  &= ~0x01;                      // TX no pull
	P2PH  |= 0x02;                       // RX pull-up

	OTCON |= 0xC0;                       //串行接口SSI1选择Uart1通信
	SS1CON0 = 0x50;                     //设置通信方式为模式一，允许接收
	reload = ((unsigned long int)Freq * 1000000UL) / baud;
	SS1CON1 = (unsigned char)reload;        //波特率低位
	SS1CON2 = (unsigned char)(reload >> 8); //波特率高位
	SS1CON0 &= 0xFC;                     //清 RI/TI，丢掉上电脏数据
	IE1 |= 0x01;                        //开启SSI中断
	EA = 1;
}

/*****************************************************
*函数名称：bit Uart1_Read(uint8_t *dat)
*函数功能：从串口1接收缓冲取出一字节
*入口参数：dat-存放读到的字节
*出口参数：1 读到数据，0 缓冲为空
*****************************************************/
bit Uart1_Read(uint8_t *dat)
{
	if(Uart1_RxHead == Uart1_RxTail)
	{
		return 0;
	}
	*dat = Uart1_RxBuf[Uart1_RxTail];
	Uart1_RxTail = (Uart1_RxTail + 1) & (UART1_RX_SIZE - 1);
	return 1;
}

/*****************************************************
*函数名称：void TWI1_Init(void)
*函数功能：TWI1初始化
*入口参数：void
*出口参数：void
*****************************************************/
void TWI1_Init(void)
{
	OTCON |= 0x80;    //选择TWI1模式
	SS1CON0 = 0x80;  // ---- x---  0为不允许接收，1为允许接收
	SS1CON1 = 0x01;  //xxxx xxxy  x为地址寄存器，y为0禁止/1允许通用地址响应
	IE1 |= 0x01;
	EA = 1;
}

/*****************************************************
*函数名称：void SPI1_Init(void)
*函数功能：SPI1初始化
*入口参数：void
*出口参数：void
*****************************************************/
void SPI1_Init(void)
{
	OTCON |= 0X40;     //选择SPI1模式
#if (SPI1_Mode == 0)	
 {  SS1CON0 = 0x23; }   //设置SPI为主设备，SCK空闲时间为低电平，SCK周期第一沿采集数据，时钟速率为Fsys/512  
#elif (SPI1_Mode == 1)	
 {  SS1CON0 = 0x03;   }   //设置SPI为从设备，SCK空闲时间为低电平，SCK周期第一沿采集数据，时钟速率为Fsys/512  
#endif 	
	SS1CON1 = 0x00;   //不允许发送中断
	SS1CON0 |= 0x80;  //开启SPI1
	IE1 |= 0x01;
	EA = 1;
   
}


/*****************************************************
*函数名称：void TWI1/SPI1/UART1_Int() interrupt 7
*函数功能：SSI中断函数
*入口参数：void
*出口参数：void
*****************************************************/
#if (SSI1_Mode == Uart1)
void Uart1_Int() interrupt 7   //Uart1中断函数
{
	if(SS1CON0&0x02)    //发送标志位判断
	{
		SS1CON0 &= 0xFD;
		Uart1SendFlag = 1;
	}
	if((SS1CON0&0x01))  //接收标志位判断
	{
		uint8_t next;
		uint8_t byte;
		SS1CON0 &= 0xFE;
		byte = SS1DAT;
		next = (Uart1_RxHead + 1) & (UART1_RX_SIZE - 1);
		if(next != Uart1_RxTail)
		{
			Uart1_RxBuf[Uart1_RxHead] = byte;
			Uart1_RxHead = next;
		}
		Uart1ReceiveFlag = 1;
	}	
}
#endif

#if (SSI1_Mode == TWI1)
void TWI1_Int() interrupt 7     //TWI中断函数
{
	if(SS1CON0&0x40)
	{
		SS1CON0 &= 0xbf;  //中断清零
		TWI1Flag = 1;
	}	
}
#endif 

#if (SSI1_Mode == SPI1)
void SPI1INT(void) interrupt 7    //SPI中断函数
{	  
	if(SS1CON1&0X08)    //发送缓存器空标志判断
	{
		SS1CON1 &= ~0X08;
	}
	if(SS1CON1&0X80)    //数据传输标志位判断
	{
		SS1CON1 &= ~0X80;
		SPI1Flag = 1;
	}
}
#endif 