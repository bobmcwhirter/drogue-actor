#![macro_use]
#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]

use core::future::Future;
use std::marker::PhantomData;
use std::str::{from_utf8, from_utf8_unchecked};
use drogue_actor::*;
use embassy::time::{Duration, Timer};
use embassy::util::Forever;
use log::info;
use drogue_actor::actor::{Actor, ActorContext, Address, Inbox, Protocol};

pub struct ReadWriteProtocol;

impl Protocol for ReadWriteProtocol {
    type Message<'m> = ReadWriteCommand<'m>;
    type Response = usize;
}

#[derive(Debug)]
pub enum ReadWriteCommand<'d> {
    Write(&'d [u8; 16]),
    Read(&'d mut [u8; 16]),
}

struct ReadWriteActor;

impl Actor<ReadWriteProtocol> for ReadWriteActor {
    type StartFuture<'f, I: Inbox<ReadWriteProtocol>> = impl Future<Output=()>
    where Self: 'f;

    fn run<'f, I: Inbox<ReadWriteProtocol>>(&'f mut self, mut inbox: I) -> Self::StartFuture<'f, I> {
        async move {
            println!("hey!");
            let mut data = [0; 16];

            loop {
                let mut message = inbox.next().await;
                if let Some(mut message) = message {
                    match *message {
                        ReadWriteCommand::Write(src) => {
                            for (data, src) in data.iter_mut().zip(src.iter()) {
                                *data = *src;
                            }
                            message.respond(42);
                        }
                        ReadWriteCommand::Read(ref mut dst) => {
                            for (data, dst) in data.iter().zip(dst.iter_mut()) {
                                *dst = *data;
                            }
                            message.respond(24);
                        }
                    }
                }
            }
        }
    }
}

pub struct NotifyProtocol;

pub enum NotifyCommand {
    Foo,
    Bar,
}

impl Protocol for NotifyProtocol {
    type Message<'m> = NotifyCommand;
}

struct NotifyActor;

impl Actor<NotifyProtocol> for NotifyActor {
    type StartFuture<'f, I: Inbox<NotifyProtocol>> = impl Future<Output=()>
    where Self: 'f;

    fn run<'f, I: Inbox<NotifyProtocol>>(&'f mut self, mut inbox: I) -> Self::StartFuture<'f, I> {
        async move {
            loop {
                println!("loop notify");
                let mut message = inbox.next().await;
                if let Some(message) = message {
                    match *message {
                        NotifyCommand::Foo => {
                            println!("foo");
                        }
                        NotifyCommand::Bar => {
                            println!("bar");
                        }
                    }
                }
            }
        }
    }
}

#[embassy::main]
async fn main(spawner: embassy::executor::Spawner) {
    static READ_WRITE: ActorContext<ReadWriteProtocol, ReadWriteActor> = ActorContext::new();
    static NOTIFY: ActorContext<NotifyProtocol, NotifyActor, 3> = ActorContext::new();

    let read_write = READ_WRITE.initialize();
    READ_WRITE.mount(spawner, ReadWriteActor);

    let notify = NOTIFY.initialize();
    NOTIFY.mount(spawner, NotifyActor);

    let response = read_write.request(ReadWriteCommand::Write(b"bob mcwhirter...")).await;
    println!("response from WRITE {:?}", response);
    let mut data = [0;16];
    let response = read_write.request(ReadWriteCommand::Read(&mut data)).await;
    println!("response from READ {:?}", response);
    println!("done: {:?}", unsafe { from_utf8_unchecked(&data) } );
    notify.notify(NotifyCommand::Foo);
    notify.notify(NotifyCommand::Bar);
    notify.notify(NotifyCommand::Bar);
    notify.notify(NotifyCommand::Bar);
}
