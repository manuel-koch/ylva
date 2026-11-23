// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use signal_hook::consts::{SIGINT, SIGTERM};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(super) struct Interrupt {
    flag: Arc<AtomicBool>,
    handlers: Vec<signal_hook::SigId>,
}

impl Interrupt {
    pub fn install() -> io::Result<Self> {
        let mut interrupt = Self {
            flag: Arc::default(),
            handlers: Vec::new(),
        };
        for signal in [SIGINT, SIGTERM] {
            interrupt.handlers.push(signal_hook::flag::register(
                signal,
                Arc::clone(&interrupt.flag),
            )?);
        }
        Ok(interrupt)
    }

    pub fn requested(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        for &handler in &self.handlers {
            signal_hook::low_level::unregister(handler);
        }
    }
}

pub(super) fn check(interrupted: &impl Fn() -> bool) -> io::Result<()> {
    if interrupted() {
        // `io::copy` retries ErrorKind::Interrupted.
        return Err(io::Error::other("export interrupted"));
    }
    Ok(())
}

pub(super) struct InterruptibleReader<'a, R, F> {
    source: R,
    interrupted: &'a F,
}

impl<'a, R, F> InterruptibleReader<'a, R, F> {
    pub fn new(source: R, interrupted: &'a F) -> Self {
        Self {
            source,
            interrupted,
        }
    }
}

impl<R: Read, F: Fn() -> bool> Read for InterruptibleReader<'_, R, F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        check(self.interrupted)?;
        self.source.read(buffer)
    }
}
