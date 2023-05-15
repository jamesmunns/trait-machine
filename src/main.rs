#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

use core::fmt::Debug;
use std::{any::type_name, time::Duration};
use tokio::sync::mpsc::{channel, Receiver, Sender};

//
// The trait specifies all the "state transitions" of both devices
//

trait TraitMachine {
    const SECTOR_SIZE: usize;
    const CHUNK_SIZE: usize;

    fn next_sector(&mut self) -> Option<usize>;

    // These are all the joint state transitions, basically?
    async fn start(&mut self) -> Result<usize, ()>;
    async fn erase_sector(&mut self, start: usize, len: usize) -> Result<(), ()>;
    async fn write_next_chunk(&mut self) -> Result<usize, ()>;
    async fn boot(&mut self) -> Result<(), ()>;
    async fn abort(&mut self) -> Result<(), ()>;
}

//
// These functions are the actual "state logic", shared by both the host and
// the client.
//

async fn bootload<TM: TraitMachine>(tm: &mut TM) -> Result<(), ()> {
    // Silly top level function just so we can guarantee an abort message is sent
    // whenever there is an inner error.
    match bootload_inner(tm).await {
        Ok(()) => Ok(()),
        Err(()) => {
            let _ = tm.abort().await?;
            Err(())
        }
    }
}

async fn bootload_inner<TM: TraitMachine>(tm: &mut TM) -> Result<(), ()> {
    let name = type_name::<TM>();
    println!("{name} STARTING");
    let image_end = tm.start().await?;

    while let Some(start) = tm.next_sector() {
        println!("{name} ERASING");
        tm.erase_sector(start, TM::SECTOR_SIZE).await?;
        let sector_end = start + TM::SECTOR_SIZE;
        let mut now = start;
        while (now < sector_end) && (now < image_end) {
            println!("{name} WRITING");
            now += tm.write_next_chunk().await?;
        }
    }

    println!("{name} BOOTING");
    tm.boot().await?;

    Ok(())
}

//
// These are the "wire types" for H->C and C->H comms
//

#[derive(Debug)]
enum Host2Client {
    Start { total_size: usize },
    EraseSector { addr: usize, len: usize },
    WriteData { addr: usize, data: Vec<u8> },
    Boot,
    Abort,
}

#[derive(Debug)]
enum Client2Host {
    ErrorReset,
    Starting,
    ChunkWritten,
    SectorErased,
    Booting,
}

//
// This is the host - it is driving the state machine.
//
// This is a vaguely RPC-like construct.
//

struct Host {
    image: Vec<u8>,
    channel: Bidir<Host2Client, Client2Host>,
    position: usize,
}

impl TraitMachine for Host {
    const SECTOR_SIZE: usize = 4096;
    const CHUNK_SIZE: usize = 256;

    fn next_sector(&mut self) -> Option<usize> {
        if self.position < self.image.len() {
            let val = self.position;
            Some(val)
        } else {
            None
        }
    }

    async fn start(&mut self) -> Result<usize, ()> {
        self.channel
            .send(Host2Client::Start {
                total_size: self.image.len(),
            })
            .await?;
        match self.channel.recv().await? {
            Client2Host::Starting => Ok(self.image.len()),
            _ => Err(()),
        }
    }

    async fn erase_sector(&mut self, start: usize, len: usize) -> Result<(), ()> {
        assert_eq!(len, Self::SECTOR_SIZE);
        self.channel
            .send(Host2Client::EraseSector { addr: start, len })
            .await?;
        match self.channel.recv().await? {
            Client2Host::SectorErased => Ok(()),
            _ => Err(()),
        }
    }

    async fn write_next_chunk(&mut self) -> Result<usize, ()> {
        if self.image.len() <= self.position {
            self.position += Self::CHUNK_SIZE;
            Ok(Self::CHUNK_SIZE)
        } else {
            let remain = &self.image[self.position..][..Self::CHUNK_SIZE];
            let data = remain.iter().copied().collect();
            self.channel
                .send(Host2Client::WriteData {
                    addr: self.position,
                    data,
                })
                .await?;
            self.position += Self::CHUNK_SIZE;
            match self.channel.recv().await? {
                Client2Host::ChunkWritten => Ok(Self::CHUNK_SIZE),
                _ => Err(()),
            }
        }
    }

    async fn boot(&mut self) -> Result<(), ()> {
        self.channel.send(Host2Client::Boot).await?;
        match self.channel.recv().await? {
            Client2Host::Booting => Ok(()),
            _ => Err(()),
        }
    }

    async fn abort(&mut self) -> Result<(), ()> {
        self.channel.send(Host2Client::Abort).await?;
        Ok(())
    }
}

//
// This is the client. It is being commanded by the host
//

struct Client {
    position: usize,
    image_len: Option<usize>,
    flash: Vec<u8>,
    channel: Bidir<Client2Host, Host2Client>,
}

impl TraitMachine for Client {
    const SECTOR_SIZE: usize = 4096;
    const CHUNK_SIZE: usize = 256;

    fn next_sector(&mut self) -> Option<usize> {
        let in_ttl_range = self.position < Self::TOTAL_SIZE;
        let in_img_range = self.image_len.map(|len| self.position < len)?;

        if in_ttl_range && in_img_range {
            let val = self.position;
            Some(val)
        } else {
            None
        }
    }

    async fn start(&mut self) -> Result<usize, ()> {
        match self.channel.recv().await? {
            Host2Client::Start { total_size } if total_size <= Self::TOTAL_SIZE => {
                self.image_len = Some(total_size);
                self.channel.send(Client2Host::Starting).await?;
                Ok(total_size)
            }
            _ => Err(()),
        }
    }

    async fn erase_sector(&mut self, sector_start: usize, sector_len: usize) -> Result<(), ()> {
        match self.channel.recv().await? {
            Host2Client::EraseSector { addr, len } => {
                let exp_pos = addr == sector_start;
                let exp_len = len == sector_len;
                if !(exp_pos && exp_len) {
                    return Err(());
                }
            }
            _ => return Err(()),
        }

        self.sector_erase(sector_start, sector_len).await?;
        self.channel.send(Client2Host::SectorErased).await?;
        Ok(())
    }

    async fn write_next_chunk(&mut self) -> Result<usize, ()> {
        match self.channel.recv().await? {
            Host2Client::WriteData { addr, data } => {
                if addr != self.position {
                    return Err(());
                }
                if data.len() != Self::CHUNK_SIZE {
                    return Err(());
                }
                self.chunk_write(addr, &data).await?;
                self.position += Self::CHUNK_SIZE;
                self.channel.send(Client2Host::ChunkWritten).await?;
                Ok(Self::CHUNK_SIZE)
            }
            _ => return Err(()),
        }
    }

    async fn boot(&mut self) -> Result<(), ()> {
        match self.channel.recv().await? {
            Host2Client::Boot => {}
            _ => return Err(()),
        }
        self.channel.send(Client2Host::Booting).await?;
        println!("Client Booted :)");
        Ok(())
    }

    async fn abort(&mut self) -> Result<(), ()> {
        let _ = self.channel.send(Client2Host::ErrorReset).await;
        Ok(())
    }
}

//
// These are fake "inherent methods" that would normally exist, like erasing/writing
// the flash memory locally.
//
// Not part of the state machine directly, but are the "side effects" of state transitions
// that are useful.
impl Client {
    const TOTAL_SIZE: usize = 32 * 1024;

    async fn sector_erase(&mut self, start: usize, len: usize) -> Result<(), ()> {
        println!("Totally erasing real flash...");
        self.flash
            .get_mut(start..(start + len))
            .ok_or(())?
            .iter_mut()
            .for_each(|b| *b = 0xFF);
        Ok(())
    }

    async fn chunk_write(&mut self, start: usize, data: &[u8]) -> Result<(), ()> {
        let len = data.len();
        self.flash
            .get_mut(start..(start + len))
            .ok_or(())?
            .iter_mut()
            .zip(data.iter())
            .for_each(|(b, d)| {
                assert_eq!(*b, 0xFF, "WRITING NON ERASED FLASH");
                *b = *d;
            });
        Ok(())
    }
}

//
// Demonstration function
//

#[tokio::main(flavor = "current_thread")]
pub async fn main() {
    let image = vec![0x42; 15 * 1024];
    let flash = vec![0x00; Client::TOTAL_SIZE];

    let h2c = channel(4);
    let c2h = channel(4);

    let host = Host {
        image,
        channel: Bidir {
            to: h2c.0,
            from: c2h.1,
        },
        position: 0,
    };
    let client = Client {
        flash,
        channel: Bidir {
            to: c2h.0,
            from: h2c.1,
        },
        position: 0,
        image_len: None,
    };

    let ctask = tokio::task::spawn(async move {
        let mut client = client;
        bootload(&mut client).await.unwrap();
        println!("Client Done!");
        tokio::time::sleep(Duration::from_millis(10)).await;
        client
    });

    let htask = tokio::task::spawn(async move {
        let mut host = host;
        bootload(&mut host).await.unwrap();
        println!("Host Done!");
        tokio::time::sleep(Duration::from_millis(10)).await;
        host
    });

    let client = ctask.await.unwrap();
    let host = htask.await.unwrap();

    assert_eq!(&host.image, &client.flash[..host.image.len()]);
    println!("Image check passed :)");
}

// Helper channel type
struct Bidir<TO, FROM> {
    to: Sender<TO>,
    from: Receiver<FROM>,
}

impl<TO: Debug, FROM: Debug> Bidir<TO, FROM> {
    async fn send(&mut self, to: TO) -> Result<(), ()> {
        // println!("sending: {to:?}");
        self.to.send(to).await.map_err(drop)
    }

    async fn recv(&mut self) -> Result<FROM, ()> {
        self.from.recv().await.ok_or(())
    }
}
