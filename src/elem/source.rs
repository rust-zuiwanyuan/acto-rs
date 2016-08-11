extern crate lossyq;
use self::lossyq::spsc::{Sender, channel};
use super::super::common::{Task, Reporter, Message, Schedule, IdentifiedReceiver, Direction, new_id};

pub trait Source {
  type OutputType : Send;

  fn process(
    &mut self,
    output: &mut Sender<Message<Self::OutputType>>) -> Schedule;
}

pub struct SourceWrap<Output: Send> {
  name       : String,
  state      : Box<Source<OutputType=Output>+Send>,
  output_tx  : Sender<Message<Output>>,
}

impl<Output: 'static+Send> Task for SourceWrap<Output> {
  fn execute(&mut self, reporter: &mut Reporter) -> Schedule {
    // TODO : make this nicer. repetitive for all elems!
    let msg_id = self.output_tx.seqno();
    let retval = self.state.process(&mut self.output_tx);
    let new_msg_id = self.output_tx.seqno();
    if msg_id != new_msg_id {
      reporter.message_sent(0, new_msg_id);
    }
    retval
  }
  fn name(&self) -> &String { &self.name }
}

pub fn new<Output: Send>(
    name            : &str,
    output_q_size   : usize,
    source          : Box<Source<OutputType=Output>+Send>)
      -> (Box<SourceWrap<Output>>, Box<Option<IdentifiedReceiver<Output>>>)
{
  let (output_tx, output_rx) = channel(output_q_size);

  (
    Box::new(
      SourceWrap{
        name        : String::from(name),
        state       : source,
        output_tx   : output_tx,
      }
    ),
    Box::new(
      Some(
          IdentifiedReceiver{
            id:     new_id(String::from(name), Direction::Out, 0),
            input:  output_rx,
          }
        )
    )
  )
}