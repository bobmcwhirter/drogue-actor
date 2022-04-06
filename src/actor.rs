use core::borrow::Borrow;
use core::cell::UnsafeCell;
use core::future::Future;
use core::marker::PhantomData;
use core::ops::Deref;
use core::pin::Pin;
use core::task::{Context, Poll};
use embassy::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy::blocking_mutex::NoopMutex;
use embassy::channel::mpsc::{split, Channel, Receiver, Sender};
use embassy::channel::signal::Signal;
use embassy::executor::raw::TaskStorage;
use embassy::executor::Spawner;
use embassy::util::Forever;
use std::ops::DerefMut;

pub trait System {}

pub trait Protocol {
    type Message<'m>;
    type Response: Send + Copy = ();
}

pub struct RequestMessage<P: Protocol>
where
    P::Message<'static>: 'static,
{
    inner: &'static mut P::Message<'static>,
    completion: *const Signal<Option<P::Response>>,
}

impl<P: Protocol> RequestMessage<P> {
    pub fn new(message: &'static mut P::Message<'static>, completion: &Signal<Option<P::Response>>) -> Self {
        Self {
            inner: message,
            completion,
        }
    }
}

pub struct NotifyMessage<P: Protocol>
{
    inner: P::Message<'static>,
}

impl<P: Protocol> NotifyMessage<P>
{
    pub fn new(message: P::Message<'static>) -> Self {
        Self { inner: message }
    }
}

pub enum ActorMessage<P: Protocol>
where
    P::Message<'static>: 'static,
{
    Request(RequestMessage<P>),
    Notify(NotifyMessage<P>),
}

pub struct ActorContext<P: Protocol, A: Actor<P>, const N: usize = 1>
where
    P: 'static,
    A: 'static,
{
    channel: UnsafeCell<Channel<NoopRawMutex, ActorMessage<P>, N>>,
    sender: UnsafeCell<Option<Sender<'static, NoopRawMutex, ActorMessage<P>, N>>>,
    receiver: UnsafeCell<Option<Receiver<'static, NoopRawMutex, ActorMessage<P>, N>>>,
    actor: Forever<A>,
    task: TaskStorage<A::StartFuture<'static, Receiver<'static, NoopRawMutex, ActorMessage<P>, N>>>,
}

unsafe impl<P: Protocol, A: Actor<P>, const N: usize> Sync for ActorContext<P, A, N> {}

impl<P: Protocol, A: Actor<P>, const N: usize> ActorContext<P, A, N> {
    pub const fn new() -> Self {
        let mut channel = Channel::new();

        ActorContext {
            channel: UnsafeCell::new(channel),
            sender: UnsafeCell::new(None),
            receiver: UnsafeCell::new(None),
            actor: Forever::new(),
            task: TaskStorage::new(),
        }
    }

    pub fn initialize(&self) -> Address<P> {
        unsafe {
            let (sender, receiver) = split(&mut *self.channel.get());
            (&mut *self.sender.get()).replace(sender);
            (&mut *self.receiver.get()).replace(receiver);
            let handler: *const dyn ChannelHandler<_> = (&*self.sender.get()).as_ref().unwrap();
            let handler = unsafe { core::mem::transmute(handler) };
            Address { handler }
        }
    }

    pub fn mount(&'static self, spawner: Spawner, actor: A) {
        let actor = self.actor.put(actor);

        let inbox = unsafe { &mut *self.receiver.get() }.take().unwrap();

        let future = actor.run(inbox);

        spawner.spawn(self.task.spawn(move || future));
    }
}

pub trait ChannelHandler<P: Protocol> {
    fn send<'m>(&self, message: ActorMessage<P>);
}

impl<'a, M: RawMutex, P: Protocol, const N: usize> ChannelHandler<P>
    for Sender<'a, M, ActorMessage<P>, N>
{
    fn send<'m>(&self, message: ActorMessage<P>) {
        self.try_send(message);
    }
}

pub struct InboxRequestMessage<'m, P: Protocol> {
    pub message: &'m mut P::Message<'m>,
    completion: *const Signal<Option<P::Response>>,
    response: Option<P::Response>,
}

impl<'m, P: Protocol> InboxRequestMessage<'m, P> {

    /// Consumes self and provides a response which will be transmitted
    /// when this is subsequently dropped.
    pub fn respond(mut self, response: P::Response) {
        self.response.replace(response);
    }

}

impl<'m, P: Protocol> Deref for InboxRequestMessage<'m, P> {
    type Target = P::Message<'m>;

    fn deref(&self) -> &Self::Target {
        self.message
    }
}

impl<'m, P: Protocol> DerefMut for InboxRequestMessage<'m, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.message
    }
}

impl<P: Protocol> Drop for InboxRequestMessage<'_, P> {
    fn drop(&mut self) {
        unsafe { &*self.completion }.signal(self.response )
    }
}

pub struct InboxNotifyMessage<'m, P: Protocol> {
    pub message: P::Message<'m>,
    _marker: PhantomData<&'m ()>,
}

impl<'m, P: Protocol> Deref for InboxNotifyMessage<'m, P> {
    type Target = P::Message<'m>;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl<'m, P: Protocol> DerefMut for InboxNotifyMessage<'m, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.message
    }
}

pub enum InboxMessage<'m, P: Protocol> {
    Request(InboxRequestMessage<'m, P>),
    Notify(InboxNotifyMessage<'m, P>),
}

impl<'m, P: Protocol> InboxMessage<'m, P> {
    pub fn respond(mut self, response: P::Response) {
        match self {
            InboxMessage::Request(inner) => {
                inner.respond(response)
            }
            InboxMessage::Notify(_) => {
                // the response just gets dropped
            }
        }
    }

}

impl<'m, P: Protocol> Deref for InboxMessage<'m, P> {
    type Target = P::Message<'m>;

    fn deref(&self) -> &Self::Target {
        match self {
            InboxMessage::Request(inner) => inner.deref(),
            InboxMessage::Notify(inner) => inner.deref(),
        }
    }
}

impl<'m, P: Protocol> DerefMut for InboxMessage<'m, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            InboxMessage::Request(inner) => inner.deref_mut(),
            InboxMessage::Notify(inner) => inner.deref_mut(),
        }
    }
}

pub trait Inbox<P: Protocol> {
    type NextFuture<'m>: Future<Output = Option<InboxMessage<'m, P>>> + 'm
    where
        Self: 'm,
        <P as Protocol>::Message<'m>: 'm;

    fn next<'m>(&'m mut self) -> Self::NextFuture<'m>;
}

impl<'a, P: Protocol, const N: usize> Inbox<P> for Receiver<'a, NoopRawMutex, ActorMessage<P>, N> {
    type NextFuture<'m> = impl Future<Output=Option<InboxMessage<'m, P>>> + 'm
    where
        Self: 'm,
        <P as Protocol>::Message<'m>: 'm;

    fn next<'m>(&'m mut self) -> Self::NextFuture<'m> {
        async move {
            let message = self.recv().await?;
            println!("got a message");
            match message {
                ActorMessage::Request(request) => {
                    let mut inner = unsafe { core::mem::transmute(request.inner) };
                    Some(InboxMessage::Request(InboxRequestMessage {
                        message: inner,
                        completion: request.completion,
                        response: None,
                    }))
                }
                ActorMessage::Notify(notify) => {
                    let mut inner: <P as Protocol>::Message<'m> = unsafe { core::mem::transmute_copy(&notify.inner) };
                    unsafe { core::mem::forget(notify.inner) };
                    //let mut inner = unsafe { core::mem::transmute(inner) };
                    Some(InboxMessage::Notify(InboxNotifyMessage {
                        message: inner,
                        _marker: PhantomData,
                    }))
                }
            }
        }
    }
}

pub trait Actor<P: Protocol> {
    type StartFuture<'f, I: Inbox<P>>: Future<Output = ()>
    where
        Self: 'f;

    fn run<'f, I: Inbox<P>>(&'f mut self, inbox: I) -> Self::StartFuture<'f, I>;
}

pub struct Address<P: Protocol> {
    handler: *const dyn ChannelHandler<P>,
}

impl<P: Protocol + 'static> Address<P> {
    pub async fn request<'m>(&'m self, mut message: P::Message<'m>) -> Option<P::Response> {
        let signal = Signal::<Option<P::Response>>::new();
        let mut message: &mut <P as Protocol>::Message<'static> =
            unsafe { core::mem::transmute(&mut message) };
        let request_message = RequestMessage::new(message, &signal);
        unsafe { &*self.handler }.send(ActorMessage::Request(request_message));
        signal.wait().await
    }

    pub fn notify(&self, mut message: P::Message<'static>)
    {
        let notify_message = NotifyMessage::new(message);
        unsafe { &*self.handler }.send(ActorMessage::Notify(notify_message));
    }
}
