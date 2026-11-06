use std::io::Write;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::thread::JoinHandle;

enum Job {
    Write(Vec<u8>, Option<SyncSender<()>>),
    Flush(SyncSender<()>),
}

/// Every byte for the terminal goes through one thread, so the render thread and the UI never interleave
/// escape sequences. Each write is a complete, absolutely positioned chunk.
pub struct TerminalWriter {
    sender: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl TerminalWriter {
    pub fn new(mut sink: Box<dyn Write + Send>) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("terminal-output".into())
            .spawn(move || {
                for job in receiver {
                    match job {
                        Job::Write(bytes, done) => {
                            // A closed terminal makes writes fail; the viewer notices through its input.
                            let _ = sink.write_all(&bytes).and_then(|()| sink.flush());
                            if let Some(done) = done {
                                let _ = done.send(());
                            }
                        }
                        Job::Flush(done) => {
                            let _ = sink.flush();
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("spawn the output thread");
        Self { sender: Some(sender), thread: Some(thread) }
    }

    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()))
    }

    pub fn write(&self, text: &str) {
        if !text.is_empty() {
            self.send(Job::Write(text.as_bytes().to_vec(), None));
        }
    }

    /// Blocks until the terminal has accepted the bytes, so a slow terminal slows the renderer down
    /// instead of letting frames pile up.
    pub fn write_and_wait(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (done, wait) = mpsc::sync_channel(1);
        self.send(Job::Write(text.as_bytes().to_vec(), Some(done)));
        let _ = wait.recv();
    }

    pub fn flush(&self) {
        let (done, wait) = mpsc::sync_channel(1);
        self.send(Job::Flush(done));
        let _ = wait.recv();
    }

    fn send(&self, job: Job) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(job);
        }
    }
}

impl Drop for TerminalWriter {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_arrive_in_order() {
        let sink = Sink::default();
        let writer = TerminalWriter::new(Box::new(sink.clone()));
        writer.write("a");
        writer.write("b");
        writer.write_and_wait("c");
        assert_eq!(sink.0.lock().unwrap().as_slice(), b"abc");
    }

    #[test]
    fn concurrent_writers_never_interleave_inside_a_chunk() {
        let sink = Sink::default();
        let writer = Arc::new(TerminalWriter::new(Box::new(sink.clone())));
        let threads: Vec<_> = (0..4)
            .map(|n| {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        writer.write(&format!("<{}>", char::from(b'a' + n).to_string().repeat(20)));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        writer.flush();
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        for chunk in text.split('>').filter(|c| !c.is_empty()) {
            let body = chunk.trim_start_matches('<');
            assert_eq!(body.len(), 20, "{chunk}");
            assert!(body.bytes().all(|b| b == body.as_bytes()[0]), "{chunk}");
        }
    }

    #[test]
    fn dropping_waits_for_pending_output() {
        let sink = Sink::default();
        {
            let writer = TerminalWriter::new(Box::new(sink.clone()));
            for _ in 0..100 {
                writer.write("x");
            }
        }
        assert_eq!(sink.0.lock().unwrap().len(), 100);
    }

    #[test]
    fn empty_writes_do_nothing() {
        let sink = Sink::default();
        let writer = TerminalWriter::new(Box::new(sink.clone()));
        writer.write("");
        writer.write_and_wait("");
        writer.flush();
        assert!(sink.0.lock().unwrap().is_empty());
    }
}
