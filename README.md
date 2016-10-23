# acto-rs library

This library is a proof of concept, never run in any production setup and is fairly untested. Use at your own risk. You were warned.

---

This library is a mixture of concepts to connect independent pieces together. These independent pieces can have:

- internal state
- typed channels to talk to others
- [scheduling rule](./src/lib.rs)

These pieces (actors) are managed by a [scheduler](./src/scheduler/mod.rs) which has a predefined number of threads to run them. The number of input and output channels are determined by the type of the actor. Possible types are:

- [Source](./src/elem/source.rs): 1 output
- [Sink](./src/elem/sink.rs): 1 input
- [Filter](./src/elem/filter.rs): 1 input, 1 output
- [Y-split](./src/elem/ysplit.rs): 1 input, 2 outputs of possible different types
- [Y-merge](./src/elem/ymerge.rs): 2 inputs of possible different types, 1 output
- [Scatter](./src/elem/scatter.rs): 1 input, multiple outputs of the same type
- [Gather](./src/elem/gather.rs): multiple inputs of the same type, 1 output

The scheduling rule determines when to run an actor:

- Loop - continously, round-robin with the other tasks of the scheduler
- OnMessage - when a message arrives to one of its input channels
- OnExternalEvent - when an external event is delivered via Scheduler::notify(..) (to integrate with MIO for example)
- Periodic(PeriodLengthInUsec) - periodically

## Usage

You need to design the topology of the components because the connections of the components need to be made before they are passed to the scheduler. The scheduler owns the components and you cannot change them afterwards from the outside.

When you pass the components to the scheduler you need to tell it how to schedule their execution based on one of the above rules. Finally you will need to start the scheduler. After you started the scheduler, you can still add new actors to it.

### The crate

```
[dependencies]
acto-rs = "0.4.0"
```

### Overview

- Implement the actors based on one of the elem traits
- Start/stop the scheduler
- Pass the actor instances to the scheduler

### Creating the actors

The actors need to implement one of the traits above. Examples:

- Source: [dummy source](/src/sample/dummy_source.rs)
- Filter: [dummy filter](/src/sample/dummy_source.rs)
- Sink: [dummy sink](/src/sample/dummy_source.rs)

#### Creating a source element

```rust
use actors::*;
use std::net::{UdpSocket, SocketAddr, Ipv4Addr, SocketAddrV4};
use std::io;
use std::mem;

pub struct ReadBytes {
  socket: UdpSocket
}

//
// this item reads 1024 bytes on UDP and passes the data forward with
// the data size and the sender address. if an error happens, then the
// error goes forward instead.
//
impl source::Source for ReadBytes {

  type OutputValue = ([u8; 1024], (usize, SocketAddr));
  type OutputError = io::Error;

  fn process(&mut self,
             output: &mut Sender<Message<Self::OutputValue, Self::OutputError>>,
             _stop: &mut bool)
  {
    output.put(|value| {
      if let &mut Some(Message::Value(ref mut item)) = value {
        // re-use the preallocated space in the queue
        match self.socket.recv_from(&mut item.0) {
          Ok((read_bytes, from_addr)) => {
            item.1 = (read_bytes, from_addr);
          },
          Err(io_error) => {
            // swap in the error message
            let error_message = Some(Message::Error(ChannelPosition(output.seqno()), io_error));
            mem::swap(value, &mut error_message);
          }
        };
      } else {
        // allocate new buffer and swap it in
        let dummy_address  = Ipv4Addr::from(0);
        let dummy_sockaddr = SocketAddrV4::new(dummy_address, 1);
        let item = ([0; 1024],(0, SocketAddr::V4(dummy_sockaddr)));

        match self.socket.recv_from(&mut item.0) {
          Ok((read_bytes, from_addr)) => {
            item.1 = (read_bytes, from_addr);
            let message = Some(Message::Value(item));
            mem::swap(value, &mut message);
          },
          Err(io_error) => {
            // swap in the error message
            let error_message = Some(Message::Error(ChannelPosition(output.seqno()), io_error));
            mem::swap(value, &mut error_message);
          }
        };
      }
    });
  }
}
```

### Starting the scheduler

The scheduler allows adding new tasks while it is running or before it was started. The scheduler can only be started/stoped once. The tasks themselves decide when to stop and they will tell it to the scheduler via the `stop` flag passed to them at execution.

```rust
let mut sched1 = Scheduler::new();
sched1.start(); // this uses one single execution thread
sched1.stop();

// to use more threads, do:
let mut sched_multi = Scheduler::new();
sched_multi.start_with_threads(12);
sched_multi.stop();
```

## License

[MIT](./LICENSE-MIT) or [Apache 2.0](./LICENSE-APACHE)
