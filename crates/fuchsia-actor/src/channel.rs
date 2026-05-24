use crate::error::ActorError;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum MessageValue {
  Json(serde_json::Value),
  Binary(Vec<u8>),
  Empty,
}

#[derive(Clone, Debug)]
pub struct Message {
  pub type_: String,
  pub value: MessageValue,
}

impl Message {
  pub fn json(type_: impl Into<String>, value: serde_json::Value) -> Self {
    Self {
      type_: type_.into(),
      value: MessageValue::Json(value),
    }
  }
}

pub struct Inbox {
  rx: mpsc::Receiver<Message>,
}

impl Inbox {
  pub fn new(rx: mpsc::Receiver<Message>) -> Self {
    Self { rx }
  }

  pub async fn recv(&mut self) -> Option<Message> {
    let msg = self.rx.recv().await;
    tracing::trace!(received = msg.is_some(), "inbox.recv");
    msg
  }
}

pub struct Emitter {
  senders: Vec<mpsc::Sender<Message>>,
}

impl Emitter {
  pub fn new(senders: Vec<mpsc::Sender<Message>>) -> Self {
    Self { senders }
  }

  pub async fn send(&self, msg: Message) -> Result<(), ActorError> {
    tracing::trace!(downstream = self.senders.len(), "emitter.send");
    match self.senders.split_last() {
      None => Ok(()),
      Some((last, rest)) => {
        for sender in rest {
          sender
            .send(msg.clone())
            .await
            .map_err(|e| ActorError::Send(e.to_string()))?;
        }
        last
          .send(msg)
          .await
          .map_err(|e| ActorError::Send(e.to_string()))
      }
    }
  }
}
