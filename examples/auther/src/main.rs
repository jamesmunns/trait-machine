#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

// State machine definition

use tokio::sync::mpsc::{Sender, Receiver, channel};
use core::fmt::Debug;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Token(u64);
#[derive(Clone, Debug)]
pub struct Challenge(String);
#[derive(PartialEq, Debug)]
pub struct Response(usize);
#[derive(Debug)]
pub struct Error;

#[derive(Clone, Debug)]
pub enum Offer {
    Authenticated(Token),
    Challenge(Challenge),
}

pub trait Auther {
    async fn check_creds(&mut self) -> Result<Offer, Error>;
    async fn challenge_response(&mut self, challenge: &Challenge) -> Result<Token, Error>;
    async fn abort(&mut self);
}

pub async fn two_factor<TM: Auther>(tm: &mut TM) -> Result<Token, Error> {
    let result = async { let outcome = tm.check_creds().await?;
        match outcome {
            Offer::Authenticated(token) => return Ok(token),
            Offer::Challenge(challenge) => tm.challenge_response(&challenge).await,
        }
    }.await;
    if result.is_err() {
        tm.abort().await;
    }
    result
}

// Wire types
#[derive(Debug)]
pub enum Client2Host {
    Authenticate { username: String, password: String },
    ChallengeResponse {
        response: Response,
    },
    ErrorReset,
}

#[derive(Debug)]
pub enum Host2Client {
    Offer(Offer),
    Token(Token),
    ErrorReset,
}

// Client impl

pub struct Client<C: TxRx> {
    comms: C,
    username: String,
    password: String,
}

impl<C> Auther for Client<C>
where
    C: TxRx<Tx = Client2Host, Rx = Host2Client>,
{
    async fn check_creds(&mut self) -> Result<Offer, Error> {
        self.comms
            .send(Client2Host::Authenticate {
                username: self.username.clone(),
                password: self.password.clone(),
            })
            .await?;
        match self.comms.receive().await? {
            Host2Client::Offer(o) => Ok(o),
            _ => Err(Error)
        }
    }

    async fn challenge_response(&mut self, challenge: &Challenge) -> Result<Token, Error> {
        let resp = challenge_responder(challenge);
        self.comms
            .send(Client2Host::ChallengeResponse { response: resp })
            .await?;
        match self.comms.receive().await? {
            Host2Client::Token(t) => Ok(t),
            _ => Err(Error)
        }
    }

    async fn abort(&mut self) {
        let _ = self.comms.send(Client2Host::ErrorReset).await;
    }
}

// Host impl

pub struct Host<C: TxRx> {
    comms: C,
}

impl<C> Auther for Host<C>
where
    C: TxRx<Tx = Host2Client, Rx = Client2Host>,
{
    async fn check_creds(&mut self) -> Result<Offer, Error> {
        match self.comms.receive().await? {
            Client2Host::Authenticate { username, password } => {
                let offer = if username == "root" && password == "hunter2" {
                    Offer::Authenticated(Token(5678))
                } else if username == "tryme" && password == "tryme" {
                    Offer::Challenge(Challenge("butts".to_string()))
                } else {
                    return Err(Error);
                };
                self.comms.send(Host2Client::Offer(offer.clone())).await?;
                Ok(offer)
            },
            _ => Err(Error),
        }
    }

    async fn challenge_response(&mut self, our_challenge: &Challenge) -> Result<Token, Error> {
        match self.comms.receive().await? {
            Client2Host::ChallengeResponse { response } if response == challenge_responder(our_challenge) => {
                let token = Token(1234);
                self.comms.send(Host2Client::Token(token.clone())).await?;
                Ok(token)
            }
            _ => Err(Error)
        }
    }

    async fn abort(&mut self) {
        let _ = self.comms.send(Host2Client::ErrorReset).await;
    }
}

pub trait TxRx {
    type Tx: 'static;
    type Rx: 'static;

    async fn send(&mut self, t: Self::Tx) -> Result<(), Error>;
    async fn receive(&mut self) -> Result<Self::Rx, Error>;
}

// Helper channel type
struct Bidir<TO, FROM> {
    to: Sender<TO>,
    from: Receiver<FROM>,
}

impl<TO: Debug + 'static, FROM: Debug + 'static> TxRx for Bidir<TO, FROM> {
    type Tx = TO;
    type Rx = FROM;

    async fn send(&mut self, to: TO) -> Result<(), Error> {
        // println!("sending: {to:?}");
        self.to.send(to).await.map_err(|_| Error)
    }

    async fn receive(&mut self) -> Result<FROM, Error> {
        self.from.recv().await.ok_or(Error)
    }
}


#[tokio::main(flavor = "current_thread")]
pub async fn main() {
    let h2c = channel(4);
    let c2h = channel(4);

    let host = Host {
        comms: Bidir {
            to: h2c.0,
            from: c2h.1,
        },
    };
    let client = Client {
        username: "tryme".into(),
        password: "tryme".into(),
        comms: Bidir {
            to: c2h.0,
            from: h2c.1,
        },
    };

    let ctask = tokio::task::spawn(async move {
        let mut client = client;
        let tok = two_factor(&mut client).await.unwrap();
        println!("Client Done! - Got token: {tok:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
        client
    });

    let htask = tokio::task::spawn(async move {
        let mut host = host;
        let tok = two_factor(&mut host).await.unwrap();
        println!("Host Done! - Sent token: {tok:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
        host
    });

    let _client = ctask.await.unwrap();
    let _host = htask.await.unwrap();
}


fn challenge_responder(c: &Challenge) -> Response {
    // lol
    Response(c.0.len())
}
