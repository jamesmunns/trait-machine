#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

use std::ops::Range;

pub enum ToBootloader<'a> {
    Start,
    StartSector {
        start: usize,
        len: usize,
    },
    Chunk {
        start: usize,
        data: &'a [u8],
        crc: u32,
    },
}

pub enum FromBootloader {
    Ok,
    ErrorReset,
    ErasedSector(usize),
    WroteChunk,

}

pub trait BlHardware {
    const TOTAL_RANGE: Range<usize>;
    const SECTOR_SIZE: usize;


    async fn erase_sector(&mut self, start: usize) -> Result<(), ()>;
    async fn write_chunk(&mut self, start: usize, data: &[u8]) -> Result<(), ()>;
    async fn read(&self, start: usize, len: usize) -> Result<&[u8], ()>;
}

pub trait BlComms {
    async fn send(&mut self, msg: FromBootloader) -> Result<(), ()>;
    async fn recv(&mut self) -> Result<ToBootloader, ()>;
}

fn main() {
    println!("Hello, world!");
}

pub struct Bootloader<Hw, Comms>
where
    Hw: BlHardware,
    Comms: BlComms,
{
    hw: Hw,
    comms: Comms,
}

impl<Hw, Comms> Bootloader<Hw, Comms>
where
    Hw: BlHardware,
    Comms: BlComms,
{
    pub async fn run(&mut self) {
        loop {
            match self.step().await {
                Ok(_) => return,
                Err(_) => {
                    let _ = self.comms.send(FromBootloader::ErrorReset).await;
                },
            }
        }
    }

    async fn step(&mut self) -> Result<(), ()> {
        // IDLE -> Operational
        match self.comms.recv().await {
            Ok(ToBootloader::Start) => {
                self.comms.send(FromBootloader::Ok).await?;
            },
            _ => return Err(()),
        }

        Ok(())
    }

    async fn step_sector(&mut self) -> Result<(), ()> {
        let (start, len) = match self.comms.recv().await {
            Ok(ToBootloader::StartSector { start, len }) => (start, len),
            _ => return Err(()),
        };

        let mut good = true;
        good &= start % Hw::SECTOR_SIZE == 0;
        good &= len == Hw::SECTOR_SIZE;
        good &= start >= Hw::TOTAL_RANGE.start;

        if !good {
            return Err(());
        }

        let end = start.checked_add(len).ok_or(())?;
        self.hw.erase_sector(start).await?;
        self.comms.send(FromBootloader::ErasedSector(start)).await?;

        let mut now = start;

        // Step chunks
        while now < end {
            let (cstart, cdata, ccrc) = match self.comms.recv().await? {
                ToBootloader::Chunk { start, data, crc } => (start, data, crc),
                _ => return Err(()),
            };
            let mut good = true;
            good &= cstart == now;
            good &= cdata.len() != 0;
            if !good {
                return Err(());
            }
            check_crc(cdata, ccrc)?;
            let new_end = now.checked_add(cdata.len()).ok_or(())?;
            self.hw.write_chunk(cstart, cdata).await?;
            now = new_end;
        }

        Ok(())
    }
}

fn check_crc(data: &[u8], crc: u32) -> Result<(), ()> {
    Ok(())
}
