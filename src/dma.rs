//! Direct Memory Access

use core::ops::Deref;

use crate::{
    pac::{self, RCC},
    rcc_en_reset,
};

#[cfg(feature = "g0")]
use crate::pac::dma;
#[cfg(not(feature = "g0"))]
use crate::pac::dma1 as dma;

// use embedded_dma::StaticWriteBuffer;

use cfg_if::cfg_if;

// todo: Several sections of this are only correct for DMA1.

#[cfg(any(feature = "l5", feature = "g0", feature = "g4"))]
#[repr(u8)]
/// See G4, Table 91: DMAMUX: Assignment of multiplexer inputs to resources.
pub(crate) enum MuxInput {
    // todo: This (on G4) goes up to 115. For now, just implement things we're likely
    // todo to use in this HAL. Make sure this is compatible beyond G4.
    Adc1 = 5,
    Dac1Ch1 = 6,
    Dac1Ch2 = 7,
    Tim6Up = 8,
    Tim7Up = 9,
    Spi1Rx = 10,
    Spi1Tx = 11,
    Spi2Rx = 12,
    Spi2Tx = 13,
    Spi3Rx = 14,
    Spi3Tx = 15,
    I2c1Rx = 16,
    I2c1Tx = 17,
    I2c2Rx = 18,
    I2c2Tx = 19,
    I2c3Rx = 20,
    I2c3Tx = 21,
    I2c4Rx = 22,
    I2c4Tx = 23,
    Usart1Rx = 24,
    Usart1Tx = 25,
    Usart2Rx = 26,
    Usart2Tx = 27,
    Usart3Rx = 28,
    Usart3Tx = 29,
    Uart4Rx = 30,
    Uart4Tx = 31,
    Uart5Rx = 32,
    Uart5Tx = 33,
    Lpuart1Rx = 34,
    Lpuart1Tx = 35,
    Adc2 = 36,
    Adc3 = 37,
    Adc4 = 38,
    Adc5 = 39,
}

#[derive(Copy, Clone)]
#[repr(u8)]
/// L4 RM, 11.4.3, "DMA arbitration":
/// The priorities are managed in two stages:
/// • software: priority of each channel is configured in the DMA_CCRx register, to one of
/// the four different levels:
/// – very high
/// – high
/// – medium
/// – low
/// • hardware: if two requests have the same software priority level, the channel with the
/// lowest index gets priority. For example, channel 2 gets priority over channel 4.
/// Only write to this when the channel is disabled.
pub enum Priority {
    Low = 0b00,
    Medium = 0b01,
    High = 0b10,
    VeryHigh = 0b11,
}

#[derive(Copy, Clone)]
/// Represents a DMA channel to select, eg when configuring for use with a peripheral.
pub enum DmaChannel {
    C1,
    C2,
    C3,
    C4,
    C5,
    // todo: Some G0 variants have channels 6 and 7 and DMA1. (And up to 5 channels on DMA2)
    #[cfg(not(feature = "g0"))]
    C6,
    #[cfg(not(feature = "g0"))]
    C7,
    // todo: Which else have 8? Also, note that some have diff amoutns on dam1 vs 2.
    #[cfg(any(feature = "l5", feature = "g4"))]
    C8,
}

#[derive(Copy, Clone)]
#[repr(u8)]
/// Set in CCR.
/// Can only be set when channel is disabled.
pub enum Direction {
    ReadFromPeriph = 0,
    ReadFromMem = 1,
}

#[derive(Copy, Clone)]
#[repr(u8)]
/// Set in CCR.
/// Can only be set when channel is disabled.
pub enum Circular {
    Disabled = 0,
    Enabled = 1,
}

#[derive(Copy, Clone)]
#[repr(u8)]
/// Peripheral and memory increment mode. (CCR PINC and MINC bits)
/// Can only be set when channel is disabled.
pub enum IncrMode {
    // Can only be set when channel is disabled.
    Disabled = 0,
    Enabled = 1,
}

#[derive(Copy, Clone)]
#[repr(u8)]
/// Peripheral and memory increment mode. (CCR PSIZE and MSIZE bits)
/// Can only be set when channel is disabled.
pub enum DataSize {
    S8 = 0b00, // ie 8 bits
    S16 = 0b01,
    S32 = 0b10,
}

#[derive(Copy, Clone)]
/// Interrupt type. Set in CCR using TEIE, HTIE, and TCIE bits.
/// Can only be set when channel is disabled.
pub enum DmaInterrupt {
    TransferError,
    HalfTransfer,
    TransferComplete,
}

/// Reduce DRY over channels when configuring a channel's CCR.
/// We must use a macro here, since match arms balk at the incompatible
/// types of `CCR1`, `CCR2` etc.
macro_rules! set_ccr {
    ($ccr:expr, $priority:expr, $direction:expr, $circular:expr, $periph_incr:expr, $mem_incr:expr, $periph_size:expr, $mem_size:expr) => {
        // "The register fields/bits MEM2MEM, PL[1:0], MSIZE[1:0], PSIZE[1:0], MINC, PINC, and DIR
        // are read-only when EN = 1"
        $ccr.modify(|_, w| w.en().clear_bit());

        if let Circular::Enabled = $circular {
            $ccr.modify(|_, w| w.mem2mem().clear_bit());
        }

        $ccr.modify(|_, w| unsafe {
            // – the channel priority
            w.pl().bits($priority as u8);
            // – the data transfer direction
            // This bit [DIR] must be set only in memory-to-peripheral and peripheral-to-memory modes.
            // 0: read from peripheral
            w.dir().bit($direction as u8 != 0);
            // – the circular mode
            w.circ().bit($circular as u8 != 0);
            // – the peripheral and memory incremented mode
            w.pinc().bit($periph_incr as u8 != 0);
            w.minc().bit($mem_incr as u8 != 0);
            // – the peripheral and memory data size
            w.psize().bits($periph_size as u8);
            w.msize().bits($mem_size as u8);
            // – the interrupt enable at half and/or full transfer and/or transfer error
            // (We handle this using the `enable_interrupt` method below.)
            // (See `Step 5` above.)
            w.en().set_bit()
        });
    }
}

/// Reduce DRY over channels when configuring a channel's interrupts.
macro_rules! enable_interrupt {
    ($ccr:expr, $interrupt_type:expr) => {
        let originally_enabled = $ccr.read().en().bit_is_set();
        if originally_enabled {
            $ccr.modify(|_, w| w.en().clear_bit());
            while $ccr.read().en().bit_is_set() {}
        }
        match $interrupt_type {
            DmaInterrupt::TransferError => $ccr.modify(|_, w| w.teie().set_bit()),
            DmaInterrupt::HalfTransfer => $ccr.modify(|_, w| w.htie().set_bit()),
            DmaInterrupt::TransferComplete => $ccr.modify(|_, w| w.tcie().set_bit()),
        }

        if originally_enabled {
            $ccr.modify(|_, w| w.en().set_bit());
            while $ccr.read().en().bit_is_clear() {}
        }
    }
}

/// This struct is used to pass common (non-peripheral and non-use-specific) data when configuring
/// a channel.
pub struct ChannelCfg {
    priority: Priority,
    circular: Circular,
    periph_incr: IncrMode,
    mem_incr: IncrMode,
    periph_size: DataSize,
    mem_size: DataSize,
}

impl Default for ChannelCfg {
    fn default() -> Self {
        Self {
            priority: Priority::Medium, // todo: Pass pri as an arg?
            circular: Circular::Disabled, // todo?
            // Increment the buffer address, not the peripheral address.
            periph_incr: IncrMode::Disabled,
            mem_incr: IncrMode::Enabled,
            periph_size: DataSize::S8, // todo: S16 for 9-bit support?
            mem_size: DataSize::S8, // todo: S16 for 9-bit support?
        }
    }
}


pub struct Dma<D> {
    regs: D,
}

impl<D> Dma<D>
    where
        D: Deref<Target = dma::RegisterBlock>,
{
    pub fn new(regs: D, rcc: &mut RCC) -> Self {
        // todo: Enable RCC for DMA 2 etc!

        cfg_if! {
            if #[cfg(feature = "f3")] {
                rcc.ahbenr.modify(|_, w| w.dma1en().set_bit()); // no dmarst on F3.
            } else if #[cfg(feature = "g0")] {
                rcc_en_reset!(ahb1, dma, rcc);
            } else {
                rcc_en_reset!(ahb1, dma1, rcc);
            }
        }

        Self { regs }
    }

    /// Configure a DMA channel. See L4 RM 0394, section 11.4.4
    pub fn cfg_channel(
        &mut self,
        channel: DmaChannel,
        periph_addr: u32,
        mem_addr: u32,
        num_data: u16,
        direction: Direction,
        cfg: ChannelCfg,
    ) {
        // todo: Consider a config struct you can impl default with, instead
        // todo of all these args. Or maybe 2 configs: One for read, the other for write.

        // The following sequence is needed to configure a DMA channel x:
        // 1. Set the peripheral register address in the DMA_CPARx register.
        // The data is moved from/to this address to/from the memory after the peripheral event,
        // or after the channel is enabled in memory-to-memory mode.

        unsafe {
            match channel {
                DmaChannel::C1 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch1.par;
                        } else {
                            let cpar = &self.regs.cpar1;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                DmaChannel::C2 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch2.par;
                        } else {
                            let cpar = &self.regs.cpar2;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                DmaChannel::C3 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch3.par;
                        } else {
                            let cpar = &self.regs.cpar3;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                DmaChannel::C4 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch4.par;
                        } else {
                            let cpar = &self.regs.cpar4;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                DmaChannel::C5 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch5.par;
                        } else {
                            let cpar = &self.regs.cpar5;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C6 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch6.par;
                        } else {
                            let cpar = &self.regs.cpar6;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C7 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cpar = &self.regs.ch7.par;
                        } else {
                            let cpar = &self.regs.cpar7;
                        }
                    }
                    cpar.write(|w| w.bits(periph_addr));
                }
                #[cfg(any(feature = "l5", feature = "g4"))]
                DmaChannel::C8 => {
                    let cpar = &self.regs.cpar8;
                    cpar.write(|w| w.bits(periph_addr));
                }
            }
        }

        // 2. Set the memory address in the DMA_CMARx register.
        // The data is written to/read from the memory after the peripheral event or after the
        // channel is enabled in memory-to-memory mode.
        unsafe {
            match channel {
                DmaChannel::C1 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch1.mar;
                        } else {
                            let cmar = &self.regs.cmar1;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                DmaChannel::C2 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch2.mar;
                        } else {
                            let cmar = &self.regs.cmar2;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                DmaChannel::C3 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch3.mar;
                        } else {
                            let cmar = &self.regs.cmar3;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                DmaChannel::C4 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch4.mar;
                        } else {
                            let cmar = &self.regs.cmar4;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                DmaChannel::C5 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch5.mar;
                        } else {
                            let cmar = &self.regs.cmar5;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C6 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch6.mar;
                        } else {
                            let cmar = &self.regs.cmar6;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C7 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cmar = &self.regs.ch7mar;
                        } else {
                            let cmar = &self.regs.cmar7;
                        }
                    }
                    cmar.write(|w| w.bits(mem_addr));
                }
                #[cfg(any(feature = "l5", feature = "g4"))]
                DmaChannel::C8 => {
                    let cmar = &self.regs.cmar8;
                    cmar.write(|w| w.bits(mem_addr));
                }
            }
        }

        // 3. Configure the total number of data to transfer in the DMA_CNDTRx register.
        // After each data transfer, this value is decremented.
        unsafe {
            match channel {
                DmaChannel::C1 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch1.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr1;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                DmaChannel::C2 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch2.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr2;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                DmaChannel::C3 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch3.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr3;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                DmaChannel::C4 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch4.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr4;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                DmaChannel::C5 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch5.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr5;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C6 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch6.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr6;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                #[cfg(not(feature = "g0"))]
                DmaChannel::C7 => {
                    cfg_if! {
                        if #[cfg(any(feature = "f3", feature = "g0"))] {
                            let cndtr = &self.regs.ch7.ndtr;
                        } else {
                            let cndtr = &self.regs.cndtr7;
                        }
                    }
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
                #[cfg(any(feature = "l5", feature = "g4"))]
                DmaChannel::C8 => {
                    let cndtr = &self.regs.cndtr8;
                    cndtr.write(|w| w.ndt().bits(num_data));
                }
            }
        }

        // 4. Configure the parameters listed below in the DMA_CCRx register:
        // (These are listed below by their corresponding reg write code)

        // todo: See note about sep reg writes to disable channel, and when you need to do this.

        // 5. Activate the channel by setting the EN bit in the DMA_CCRx register.
        // A channel, as soon as enabled, may serve any DMA request from the peripheral connected
        // to this channel, or may start a memory-to-memory block transfer.
        // Note: The two last steps of the channel configuration procedure may be merged into a single
        // access to the DMA_CCRx register, to configure and enable the channel.
        // When a channel is enabled and still active (not completed), the software must perform two
        // separate write accesses to the DMA_CCRx register, to disable the channel, then to
        // reprogram the channel for another next block transfer.
        // Some fields of the DMA_CCRx register are read-only when the EN bit is set to 1

        // (later): The circular mode must not be used in memory-to-memory mode. Before enabling a
        // channel in circular mode (CIRC = 1), the software must clear the MEM2MEM bit of the
        // DMA_CCRx register. When the circular mode is activated, the amount of data to transfer is
        // automatically reloaded with the initial value programmed during the channel configuration
        // phase, and the DMA requests continue to be served

        // (See remainder of steps in `set_ccr()!` macro.

        // todo: Let user set mem2mem mode?

        match channel {
            DmaChannel::C1 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch1.cr;
                    } else {
                        let ccr = &self.regs.ccr1;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            DmaChannel::C2 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch2.cr;
                    } else {
                        let ccr = &self.regs.ccr2;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            DmaChannel::C3 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch3.cr;
                    } else {
                        let ccr = &self.regs.ccr3;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            DmaChannel::C4 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch4.cr;
                    } else {
                        let ccr = &self.regs.ccr4;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            DmaChannel::C5 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch5.cr;
                    } else {
                        let ccr = &self.regs.ccr5;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C6 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch6.cr;
                    } else {
                        let ccr = &self.regs.ccr6;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C7 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch7.cr;
                    } else {
                        let ccr = &self.regs.ccr7;
                    }
                }
                set_ccr!(
                    ccr,
                    cfg.priority,
                    direction,
                    cfg.circular,
                    cfg.periph_incr,
                    cfg.mem_incr,
                    cfg.periph_size,
                    cfg.mem_size
                );
            }
            #[cfg(any(feature = "l5", feature = "g4"))]
            DmaChannel::C8 => {
                let mut ccr = &self.regs.ccr8;
                set_ccr!(
                    ccr,
                    priority,
                    direction,
                    circular,
                    periph_incr,
                    mem_incr,
                    periph_size,
                    mem_size
                );
            }
        }
    }

    pub fn stop(&mut self, channel: DmaChannel) {
        // L4 RM:
        // Once the software activates a channel, it waits for the completion of the programmed
        // transfer. The DMA controller is not able to resume an aborted active channel with a possible
        // suspended bus transfer.
        // To correctly stop and disable a channel, the software clears the EN bit of the DMA_CCRx
        // register. The software secures that no pending request from the peripheral is served by the
        // DMA controller before the transfer completion. The software waits for the transfer complete
        // or transfer error interrupt.
        // When a channel transfer error occurs, the EN bit of the DMA_CCRx register is cleared by
        // hardware. This EN bit can not be set again by software to re-activate the channel x, until the
        // TEIFx bit of the DMA_ISR register is set

        match channel {
            DmaChannel::C1 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch1.cr;
                    } else {
                        let ccr = &self.regs.ccr1;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            DmaChannel::C2 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch2.cr;
                    } else {
                        let ccr = &self.regs.ccr2;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            DmaChannel::C3 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch3.cr;
                    } else {
                        let ccr = &self.regs.ccr3;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            DmaChannel::C4 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch4.cr;
                    } else {
                        let ccr = &self.regs.ccr4;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            DmaChannel::C5 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch5.cr;
                    } else {
                        let ccr = &self.regs.ccr5;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C6 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch6.cr;
                    } else {
                        let ccr = &self.regs.ccr6;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C7 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch7.cr;
                    } else {
                        let ccr = &self.regs.ccr7;
                    }
                }
                ccr.modify(|_, w| w.en().clear_bit())
            }
            #[cfg(any(feature = "l5", feature = "g4"))]
            DmaChannel::C8 => {
                let ccr = &self.regs.ccr8;
                ccr.modify(|_, w| w.en().clear_bit())
            }
        };

        // todo: Check for no pending request and transfer complete/error
    }

    #[cfg(feature = "l4")] // Only on L4
    /// Select which peripheral on a given channel we're using.
    /// See L44 RM, Table 41.
    pub fn channel_select(&mut self, channel: DmaChannel, selection: u8) {
        if selection > 7 {
            // Alternatively, we could use an enum
            panic!("CSEL must be 0 - 7")
        }
        match channel {
            DmaChannel::C1 => self.regs.cselr.modify(|_, w| w.c1s().bits(selection)),
            DmaChannel::C2 => self.regs.cselr.modify(|_, w| w.c2s().bits(selection)),
            DmaChannel::C3 => self.regs.cselr.modify(|_, w| w.c3s().bits(selection)),
            DmaChannel::C4 => self.regs.cselr.modify(|_, w| w.c4s().bits(selection)),
            DmaChannel::C5 => self.regs.cselr.modify(|_, w| w.c5s().bits(selection)),
            DmaChannel::C6 => self.regs.cselr.modify(|_, w| w.c6s().bits(selection)),
            DmaChannel::C7 => self.regs.cselr.modify(|_, w| w.c7s().bits(selection)),
        }
    }

    #[cfg(any(feature = "l5", feature = "g0", feature = "g4"))]
    /// Configure a specific DMA channel to work with a specific peripheral.
    pub fn mux(&mut self, channel: DmaChannel, selection: u8, mux: &pac::DMAMUX) {
        // Note: This is similar in API and purpose to `channel_select` above,
        // for different families. We're keeping it as a separate function instead
        // of feature-gating within the same function so the name can be recognizable
        // from the RM etc.
        unsafe {
            #[cfg(not(any(feature = "g070", feature = "g071", feature = "g081")))]
            match channel {
                DmaChannel::C1 => mux.c1cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C2 => mux.c2cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C3 => mux.c3cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C4 => mux.c4cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C5 => mux.c5cr.modify(|_, w| w.dmareq_id().bits(selection)),
                #[cfg(not(feature = "g0"))]
                DmaChannel::C6 => mux.c6cr.modify(|_, w| w.dmareq_id().bits(selection)),
                #[cfg(not(feature = "g0"))]
                DmaChannel::C7 => mux.c7cr.modify(|_, w| w.dmareq_id().bits(selection)),
                #[cfg(any(feature = "l5", feature = "g4"))]
                DmaChannel::C8 => mux.c8cr.modify(|_, w| w.dmareq_id().bits(selection)),
            }
            #[cfg(any(feature = "g070", feature = "g071", feature = "g081"))]
            match channel {
                DmaChannel::C1 => mux.dmamux_c1cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C2 => mux.dmamux_c2cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C3 => mux.dmamux_c3cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C4 => mux.dmamux_c4cr.modify(|_, w| w.dmareq_id().bits(selection)),
                DmaChannel::C5 => mux.dmamux_c5cr.modify(|_, w| w.dmareq_id().bits(selection)),
            }
        }
    }

    /// Enable a specific type of interrupt.
    pub fn enable_interrupt(&mut self, channel: DmaChannel, interrupt_type: DmaInterrupt) {
        // Can only be set when the channel is disabled.
        match channel {
            DmaChannel::C1 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch1.cr;
                    } else {
                        let ccr = &self.regs.ccr1;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            DmaChannel::C2 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch2.cr;
                    } else {
                        let ccr = &self.regs.ccr2;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            DmaChannel::C3 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch3.cr;
                    } else {
                        let ccr = &self.regs.ccr3;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            DmaChannel::C4 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch4.cr;
                    } else {
                        let ccr = &self.regs.ccr4;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            DmaChannel::C5 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch5.cr;
                    } else {
                        let ccr = &self.regs.ccr5;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C6 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch6.cr;
                    } else {
                        let ccr = &self.regs.ccr6;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            #[cfg(not(feature = "g0"))]
            DmaChannel::C7 => {
                cfg_if! {
                    if #[cfg(any(feature = "f3", feature = "g0"))] {
                        let ccr = &self.regs.ch7.cr;
                    } else {
                        let ccr = &self.regs.ccr7;
                    }
                }
                enable_interrupt!(ccr, interrupt_type);
            }
            #[cfg(any(feature = "l5", feature = "g4"))]
            DmaChannel::C8 => {
                let ccr = &self.regs.ccr8;
                enable_interrupt!(ccr, interrupt_type);
            }
        };
    }

    pub fn clear_interrupt(&mut self, channel: DmaChannel, interrupt_type: DmaInterrupt) {
        // todo: CGIFx for global interrupt flag clear. What is that? Should we impl?
        match channel {
            DmaChannel::C1 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif1().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif1().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif1().set_bit()),
            },
            DmaChannel::C2 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif2().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif2().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif2().set_bit()),
            },
            DmaChannel::C3 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif3().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif3().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif3().set_bit()),
            },
            DmaChannel::C4 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif4().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif4().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif4().set_bit()),
            },
            DmaChannel::C5 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif5().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif5().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif5().set_bit()),
            },
            #[cfg(not(feature = "g0"))]
            DmaChannel::C6 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif6().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif6().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif6().set_bit()),
            },
            #[cfg(not(feature = "g0"))]
            DmaChannel::C7 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif7().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif7().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif7().set_bit()),
            },
            #[cfg(any(feature = "l5", feature = "g4"))]
            DmaChannel::C8 => match interrupt_type {
                DmaInterrupt::TransferError => self.regs.ifcr.write(|w| w.cteif8().set_bit()),
                DmaInterrupt::HalfTransfer => self.regs.ifcr.write(|w| w.chtif8().set_bit()),
                DmaInterrupt::TransferComplete => self.regs.ifcr.write(|w| w.ctcif8().set_bit()),
            },
        }
    }
}
