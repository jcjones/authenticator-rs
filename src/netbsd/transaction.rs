/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::util::StateCallback;
use runloop::RunLoop;

use crate::platform::fd::Fd;
use crate::platform::monitor::Monitor;

pub struct Transaction {
    // Handle to the thread loop.
    thread: Option<RunLoop>,
}

impl Transaction {
    pub fn new<F, T>(
        timeout: u64,
        callback: StateCallback<Result<T, crate::Error>>,
        new_device_cb: F,
    ) -> Result<Self, crate::Error>
    where
        F: Fn(Fd, &dyn Fn() -> bool) + Sync + Send + 'static,
        T: 'static,
    {
        let thread = RunLoop::new_with_timeout(
            move |alive| {
                // Create a new device monitor.
                let mut monitor = Monitor::new(new_device_cb);

                // Start polling for new devices.
                try_or!(monitor.run(alive), |_| callback
                    .call(Err(crate::Error::Unknown)));

                // Send an error, if the callback wasn't called already.
                callback.call(Err(crate::Error::NotAllowed));
            },
            timeout,
        )
        .map_err(|_| crate::Error::Unknown)?;

        Ok(Self {
            thread: Some(thread),
        })
    }

    pub fn cancel(&mut self) {
        // This must never be None.
        self.thread.take().unwrap().cancel();
    }
}
