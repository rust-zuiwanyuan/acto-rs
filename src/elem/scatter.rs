extern crate lossyq;
use self::lossyq::spsc::{Sender, Receiver, channel};
use super::super::common::{Task, Reporter, Message, Schedule, IdentifiedReceiver, Direction, new_id};
use super::super::connectable::{Connectable};

pub trait Scatter {
  type InputType   : Send;
  type OutputType  : Send;

  fn process(
    &mut self,
    input:   &mut Receiver<Message<Self::InputType>>,
    output:  &mut Vec<Sender<Message<Self::OutputType>>>) -> Schedule;
}

pub struct ScatterWrap<Input: Send, Output: Send> {
  name           : String,
  state          : Box<Scatter<InputType=Input,OutputType=Output>+Send>,
  input_rx       : Option<IdentifiedReceiver<Input>>,
  output_tx_vec  : Vec<Sender<Message<Output>>>,
}

impl<Input: Send, Output: Send> Connectable for ScatterWrap<Input,Output> {
  type Input = Input;

  fn input(&mut self) -> &mut Option<IdentifiedReceiver<Input>> {
    &mut self.input_rx
  }
}

impl<Input: Send, Output: Send> Task for ScatterWrap<Input,Output> {
  fn execute(&mut self, reporter: &mut Reporter) -> Schedule {
    match &mut self.input_rx {
      &mut Some(ref mut identified) => {
        // TODO : make this nicer. repetitive for all elems!
        let mut msg_ids = vec![];
        for otx in &self.output_tx_vec {
          msg_ids.push(otx.seqno());
        }
        let retval = self.state.process(&mut identified.input,
                                        &mut self.output_tx_vec);
        let otx_slice = self.output_tx_vec.as_slice();
        let ids_slice = msg_ids.as_slice();
        for i in 0..msg_ids.len() {
          let new_msg_id = otx_slice[i].seqno();
          if ids_slice[i] != new_msg_id {
            reporter.message_sent(i, new_msg_id);
          }
        }
        retval
      },
      &mut None => Schedule::EndPlusUSec(10_000)
    }
  }
  fn name(&self) -> &String { &self.name }
}

pub fn new<Input: Send, Output: Send>(
    name            : &str,
    output_q_size   : usize,
    scatter         : Box<Scatter<InputType=Input,OutputType=Output>+Send>,
    n_channels      : usize)
      -> (Box<ScatterWrap<Input,Output>>, Vec<Box<Option<IdentifiedReceiver<Output>>>>)
{
  let mut tx_vec = vec![];
  let mut rx_vec = vec![];

  for i in 0..n_channels {
    let (output_tx, output_rx) = channel(output_q_size);
    tx_vec.push(output_tx);
    rx_vec.push(
      Box::new(
        Some(
          IdentifiedReceiver{
            id:     new_id(String::from(name), Direction::Out, i),
            input:  output_rx,
          }
        )
      )
    );
  }

  (
    Box::new(
      ScatterWrap{
        name           : String::from(name),
        state          : scatter,
        input_rx       : None,
        output_tx_vec  : tx_vec,
      }
    ),
    rx_vec
  )
}