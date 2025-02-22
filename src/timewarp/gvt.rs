// GVT/Coordinator Thread,
use std::{sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
}, thread};

use crate::worlds::SimError;

use super::{comms::{CircularBuffer, Comms, Transferable}, lp::LP, paragent::LogicalProcess};

pub struct GVT<const LPS: usize, const SIZE: usize, const SLOTS: usize, const HEIGHT: usize> {
    global_time: usize,
    terminal: usize,
    local_times: [Option<Arc<AtomicUsize>>; LPS], 
    comms: Option<Comms<LPS, SIZE>>,
    host: [[[Option<Transferable>; SIZE]; LPS]; 2],
    temp_load: Vec<(CircularBuffer<SIZE>, CircularBuffer<SIZE>)>,
    lps: [Option<LP<SLOTS, HEIGHT, SIZE>>; LPS],
    message_overflow: [Vec<Transferable>; LPS],
}

impl<const LPS: usize, const SIZE: usize, const SLOTS: usize, const HEIGHT: usize> GVT<LPS, SIZE, SLOTS, HEIGHT>{
    pub fn start_engine(terminal: usize) -> Self {
        let lps = [const { None }; LPS];
        let message_overflow: [Vec<Transferable>; LPS] = std::array::from_fn(|_| Vec::new());
        let local_times = [const { None }; LPS];
        let comms = None;
        let host: [[[Option<Transferable>; SIZE]; LPS]; 2] = std::array::from_fn(|_| std::array::from_fn(|_| std::array::from_fn(|_| None)));
        GVT {
            global_time: 0,
            local_times,
            terminal,
            comms,
            host,
            temp_load: Vec::new(),
            lps,
            message_overflow
        }
    }

    pub fn spawn_process<T: 'static>(&mut self, process: Box<dyn LogicalProcess>, timestep: f64, log_slots: usize) -> Result<(), SimError> {
        let ptr_idx = self.lps.iter().rposition(|x| x.is_none());
        if ptr_idx.is_none() {
            return Err(SimError::LPsFull);
        }
        let ptr1 = &mut self.host[0][ptr_idx.unwrap()] as *mut [Option<Transferable>; SIZE];
        let ptr2 = &mut self.host[1][ptr_idx.unwrap()] as *mut [Option<Transferable>; SIZE];

        let r1 = Arc::new(AtomicUsize::from(0));
        let w1 = Arc::new(AtomicUsize::from(0));
        let r2 = Arc::new(AtomicUsize::from(0));
        let w2 = Arc::new(AtomicUsize::from(0));

        let circ1 = CircularBuffer {
            ptr: ptr1.clone(),
            write_idx: Arc::clone(&w1),
            read_idx: Arc::clone(&r1),
        };
        let circ2 = CircularBuffer {
            ptr: ptr2.clone(),
            write_idx: Arc::clone(&w2),
            read_idx: Arc::clone(&r2),
        };
        let step = Arc::new(AtomicUsize::from(0));
        self.local_times[ptr_idx.unwrap()] = Some(Arc::clone(&step));
        let lp_comms = [CircularBuffer {ptr: ptr1, write_idx: w1, read_idx: r1 }, CircularBuffer {ptr: ptr2, write_idx: w2, read_idx: r2 }];
        let lp = LP::<SLOTS, HEIGHT, SIZE>::new::<T>(ptr_idx.unwrap(), process, timestep, step, lp_comms, log_slots);
        self.lps[ptr_idx.unwrap()] = Some(lp);
        self.temp_load.push((circ1, circ2));
        Ok(())
    }

    pub fn init_comms(&mut self) -> Result<(), SimError> {
        let len = self.temp_load.len();
        let mut comms_buffers1 = Vec::new();
        let mut comms_buffers2 = Vec::new();
        for i in 0..len {
            let pair = self.temp_load.remove(i);
            comms_buffers1.push(pair.0);
            comms_buffers2.push(pair.1);
        }
        if comms_buffers1.len() < LPS || comms_buffers2.len() < LPS {
            return Err(SimError::MismatchLPsCount);
        }
        let slc1: Result<[CircularBuffer<SIZE>; LPS], _> = comms_buffers1.try_into();
        let slc2: Result<[CircularBuffer<SIZE>; LPS], _> = comms_buffers2.try_into();
        let comms_wheel = [slc1.unwrap(), slc2.unwrap()];
        self.comms = Some(Comms::new(comms_wheel));
        for i in 0..LPS {
            self.lps[i].as_mut().unwrap().set_terminal(self.terminal as f64);
        }
        Ok(())
    }

    fn poll_times(&mut self) { 
        for i in 0..LPS {
            let ltime = self.local_times[i].as_ref().unwrap().load(Ordering::Relaxed);
            if ltime < self.global_time {
                self.global_time = ltime;
            }
        }
    }

    fn try_empty(&mut self, idx: usize) {
        let len = self.message_overflow[idx].len();
        for _ in 0..len {
            let val = self.message_overflow[idx].pop().unwrap();
            let status = self.comms.as_mut().unwrap().write(val);
            if status.is_err() {
                self.message_overflow[idx].push(status.err().unwrap());
                return;
            }
        }
    }
}

pub fn run<const LPS: usize, const SIZE: usize, const SLOTS: usize, const HEIGHT: usize>(gvt: &'static mut GVT<LPS, SIZE, SLOTS, HEIGHT>) -> Result<(), SimError> {
    let mut handles = Vec::new();
    for i in 0..LPS {
        if let Some(mut lp) = gvt.lps[i].take() {
            let handle = thread::spawn(move || {
                lp.run()
            });
            handles.push(handle);
        }
    }
    let main = {
        let comms = gvt.comms.as_mut().unwrap();
        let local_times = &gvt.local_times;
        let message_overflow = &mut gvt.message_overflow;
        let global_time = &mut gvt.global_time;
        let terminal = &mut gvt.terminal;
        thread::spawn(move || {
            loop {
                let mut min_time = usize::MAX;
                for time in local_times.iter().flatten() {
                    let ltime = time.load(Ordering::Relaxed);
                    if ltime < min_time {
                        min_time = ltime;
                    }
                }
                *global_time = if min_time == usize::MAX { 0 } else { min_time };
                if global_time >= terminal {
                    break;
                }
                for i in 0..LPS {
                    if message_overflow[i].len() > 0 {
                        let len = message_overflow[i].len();
                        for i in 0..len {
                            let val = message_overflow[i].pop().unwrap();
                            let status = comms.write(val);
                            if status.is_err() {
                                message_overflow[i].push(status.err().unwrap());
                                break;
                            }
                        }
                    }
                };
                let results = comms.poll();
                if results.is_err() {
                    return Err(SimError::PollError);
                }
                for (i, j) in results.unwrap().iter().enumerate() {
                    if *j {
                        let mut counter = 0;
                        loop {
                            if counter == SIZE {
                                break;
                            }
                            let msg = comms.read(i);
                            if msg.is_err() {
                                break;
                            }
                            let status = comms.write(msg.unwrap());
                            if status.is_err() {
                                let msg = status.err().unwrap();
                                message_overflow[msg.to()].push(msg);
                            }
                            counter += 1;
                        }
                    }
                }
            }
            Ok(())
    })};
    main.join().map_err(|_| SimError::ThreadJoinError)??;
    Ok(())
}