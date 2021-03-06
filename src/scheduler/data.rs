
use std::collections::{HashMap};
use std::sync::atomic::{AtomicUsize, AtomicBool, AtomicPtr, Ordering};
use super::super::{Task, Error, TaskId, ReceiverChannelId,
  ChannelId, SchedulingRule, PeriodLengthInUsec};
use super::{page, prv};
use std::sync::{Mutex};
use std::ptr;
use std::time::{Instant};
use libc;

pub struct SchedulerData {
  // ticker only:
  start:       Instant,
  // shared between threads
  // everything below has to be thread safe:
  max_id:      AtomicUsize,
  l1:          Vec<AtomicPtr<page::TaskPage>>,
  stop:        AtomicBool,
  time_us:     AtomicUsize,
  ids:         Mutex<HashMap<String, TaskId>>,
  unresolved:  Mutex<HashMap<String, HashMap<TaskId,Vec<ChannelId>>>>,
}

impl SchedulerData {
  fn add_l2_page(&mut self, idx: usize) {
    let array = Box::new(page::new(idx));
    let len = self.l1.len();
    if idx >= len-1 {
      // extend slice
      for _i in 0..initial_capacity() {
        self.l1.push(AtomicPtr::default());
      }
    }
    let l1_slice = self.l1.as_mut_slice();
    l1_slice[idx].store(Box::into_raw(array), Ordering::Release);
  }

  fn new() -> SchedulerData {
    let l1_size = initial_capacity();
    let mut data = SchedulerData{
      start:       Instant::now(),
      // zero ID is skipped
      max_id:      AtomicUsize::new(1),
      l1:          Vec::with_capacity(l1_size),
      stop:        AtomicBool::new(false),
      time_us:     AtomicUsize::new(0),
      ids:         Mutex::new(HashMap::new()),
      unresolved:  Mutex::new(HashMap::new()),
    };

    // fill the l1 bucket
    for _i in 0..l1_size {
      data.l1.push(AtomicPtr::default());
    }

    // add two initial l2 pages
    data.add_l2_page(0);
    data.add_l2_page(1);
    data
  }

  fn mark_conditional_task(&mut self,
                           id: TaskId)
  {
    let (l1, l2) = page::position(id.0);
    unsafe {
      let l1_ptr = self.l1.get_unchecked_mut(l1).load(Ordering::Acquire);
      if l1_ptr.is_null() == false {
        (*l1_ptr).set_conditional_exec_flag(l2);
      }
    }
  }

  fn mark_periodic_task(&mut self,
                        id: TaskId,
                        period: PeriodLengthInUsec)
  {
    let (l1, l2) = page::position(id.0);
    unsafe {
      let l1_ptr = self.l1.get_unchecked_mut(l1).load(Ordering::Acquire);
      if l1_ptr.is_null() == false {
        (*l1_ptr).set_delayed_exec(l2, period);
      }
    }
  }

  fn allocate_id_for_task(&mut self, task: &Box<Task+Send>) -> Result<TaskId, Error> {
    let mut ids = self.ids.lock().unwrap();
    if ids.contains_key(task.name()) {
      Result::Err(Error::AlreadyExists)
    } else {
      let task_id = TaskId(self.max_id.fetch_add(1, Ordering::AcqRel));
      ids.insert(task.name().clone(), task_id);
      Result::Ok(task_id)
    }
  }

  fn resolve_task_id(&self, name: &String) -> Option<TaskId> {
    let ids = self.ids.lock().unwrap();
    match ids.get(name) {
      Some(&id)  => Some(id),
      None       => None
    }
  }

  fn register_dependents(&mut self,
                         id: TaskId,
                         deps: Vec<(ChannelId, TaskId)>)
  {
    if deps.is_empty() { return; }
    let (l1, l2) = page::position(id.0);
    unsafe {
      let l1_ptr = self.l1.get_unchecked_mut(l1).load(Ordering::Acquire);
      if l1_ptr.is_null() == false {
        (*l1_ptr).set_dependents_flag(l2);
        (*l1_ptr).register_dependents(l2, deps);
      }
    }
  }

  pub fn add_task(&mut self,
                  task: Box<Task+Send>,
                  rule: SchedulingRule)
      -> Result<TaskId, Error>
  {
    let result : Result<TaskId, Error>;

    {
      // limit the scope of the global task name hash's lock
      result = self.allocate_id_for_task(&task);
      if result.is_err() { return result; }
    }

    if let Ok(task_id) = result {
      match rule {
        SchedulingRule::OnMessage => {
          let input_count = task.input_count();
          // resolve input task ids
          for i in 0..input_count {
            if let Some(ref ch_id_sender_name) = task.input_id(ReceiverChannelId(i)) {
              let ref sender_ch_id  = ch_id_sender_name.0;
              let ref sender_name   = ch_id_sender_name.1;
              // lookup sender id based on the name
              match self.resolve_task_id(&sender_name.0) {
                Some(sender_id) => {
                  // the other task that the current one depends
                  //  on is already registered.
                  self.register_dependents(sender_id, vec![(*sender_ch_id, task_id)]);
                }
                None => {
                  // the other task that the current one depends
                  //  on is not added yet. record it as unresolved:
                  let mut unresolved = self.unresolved.lock().unwrap();
                  // register that this task needs the task id of the sender
                  // - based on the sender name and chanel id
                  let dependents = unresolved.entry(sender_name.0.clone()).or_insert(HashMap::new());
                  let channels = dependents.entry(task_id).or_insert(Vec::new());
                  channels.push(*sender_ch_id);
                }
              }
            }
          }
          self.mark_conditional_task(task_id);
        },
        SchedulingRule::OnExternalEvent => {
          self.mark_conditional_task(task_id);
        },
        SchedulingRule::Periodic(period) => {
          self.mark_periodic_task(task_id, period);
        },
        // other scheduling rule types are currently ignored
        _ => {}
      }

      let output_count = task.output_count();
      let task_name    = task.name().clone();
      {
        // make sure the next bucket exists when needed
        let (l1, l2) = page::position(task_id.0);
        if l2 == 0 {
          self.add_l2_page(l1+1);
        }

        unsafe {
          let l1_ptr = self.l1.get_unchecked_mut(l1).load(Ordering::Acquire);
          if l1_ptr.is_null() == false {
            // TODO : store scheduling rule somewhere ????
            //(*l1_ptr).init_info(l2, output_count, rule);
            (*l1_ptr).store(l2, task);
          }
        }
      }

      {
        // unresolved ids if any, for other tasks
        let mut register_these : Vec<(ChannelId, TaskId)> = Vec::with_capacity(output_count);
        {
          let mut unresolved = self.unresolved.lock().unwrap();
          if let Some(dependents) = unresolved.get(&task_name) {
            for (dep_id, channels) in dependents.iter() {
              for ch in channels.iter() {
                register_these.push((*ch, *dep_id))
              }
            }
          }
          unresolved.remove(&task_name);
        }
        self.register_dependents(task_id, register_these);
      }
    }

    result
  }

  pub fn ticker(&mut self) {
    loop {
      unsafe { libc::usleep(10); }
      let diff = self.start.elapsed();
      let diff_us = diff.as_secs() as usize * 1000_000 + diff.subsec_nanos() as usize / 1000;
      self.time_us.store(diff_us, Ordering::Release);
      // check stop state
      if self.stop.load(Ordering::Acquire) {
        break;
      }
    }
  }

  pub fn entry(&mut self, id: usize) {

    let start = Instant::now();
    let mut iter = 0u64;
    let mut private_data = prv::Private::new();

    let l2_max = page::max_idx();
    loop {

      let max_id = self.max_id.load(Ordering::Acquire);
      private_data.ensure_size(max_id);

      let (l1, l2) = page::position(max_id);

      {
        let l1_slice = self.l1.as_mut_slice();

        // go through all fully filled l2 buckets
        let mut l2_max_idx = l2_max;
        for l1_idx in 0..l1 {
          let l1_ptr = l1_slice[l1_idx].load(Ordering::Acquire);
          unsafe {
            (*l1_ptr).eval(
              l2_max_idx,         // the max ID on the task page
              id,                 // the ID of the executor thread
              &mut private_data,  // thread private data
              &self.time_us       // current time
            );
          }
        }

        // take care of the last, partially filled bucket
        l2_max_idx = l2;
        for l1_idx in l1..(l1+1) {
          let l1_ptr = l1_slice[l1_idx].load(Ordering::Acquire);
          unsafe {
            (*l1_ptr).eval(
              l2_max_idx,         // the max ID on the task page
              id,                 // the ID of the executor thread
              &mut private_data,  // thread private data
              &self.time_us       // current time
            );
          }
        }
      }

      {
        let to_trigger = private_data.to_trigger();
        for t in to_trigger {
          self.schedule_exec(t);
        }
      }
      private_data.clear();

      iter += 1;

      // check stop state
      if self.stop.load(Ordering::Acquire) {
        break;
      }
    }

    if self.print_stats_enabled() {
      let diff = start.elapsed();
      let diff_ns = diff.as_secs() * 1_000_000_000 + diff.subsec_nanos() as u64;
      let ns_iter = diff_ns/iter;

      println!("#{} loop_count: {} {} ns/iter",id,iter,ns_iter);
    }
  }

  pub fn schedule_exec(&mut self, id: &TaskId) {
    let (l1, l2) = page::position(id.0);
    unsafe {
      let l1_ptr = self.l1.get_unchecked_mut(l1).load(Ordering::Acquire);
      if l1_ptr.is_null() == false {
        (*l1_ptr).schedule_exec(l2);
      }
    }
  }

  pub fn notify(&mut self, id: &TaskId) -> Result<(), Error> {
    if self.stop.load(Ordering::Acquire) {
      return Result::Err(Error::Stopping);
    }
    let max = self.max_id.load(Ordering::Acquire);
    if id.0 >= max {
      return Result::Err(Error::NonExistent);
    }
    let (l1, l2) = page::position(id.0);
    let l1_slice = self.l1.as_mut_slice();
    let l1_ptr = l1_slice[l1].load(Ordering::Acquire);
    if l1_ptr.is_null() {
      return Result::Err(Error::NonExistent);
    }
    unsafe { Ok((*l1_ptr).schedule_exec(l2)) }
  }

  pub fn stop(&mut self) {
    self.stop.store(true, Ordering::Release);
  }

  #[cfg(any(test,feature = "printstats"))]
  fn print_stats_enabled(&self) -> bool { true }

  #[cfg(not(any(test,feature = "printstats")))]
  fn print_stats_enabled(&self) -> bool { false }
}

pub fn new() -> SchedulerData {
  SchedulerData::new()
}

pub fn initial_capacity() -> usize {
  1024*1024
}

impl Drop for SchedulerData {
  fn drop(&mut self) {
    let len = self.l1.len();
    let l1_slice = self.l1.as_mut_slice();
    for i in 0..len {
      let l1_atomic_ptr = &mut l1_slice[i];
      let ptr = l1_atomic_ptr.swap(ptr::null_mut::<page::TaskPage>(), Ordering::AcqRel);
      if ptr.is_null() == false {
        // make sure we drop the pointers
        let _b = unsafe { Box::from_raw(ptr) };
      } else {
        break;
      }
    }
  }
}
