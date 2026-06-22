//! A sink block that posts the data stream from worker to main UI thread.
use rustradio::block::{Block, BlockRet};
use rustradio::stream::ReadStream;
use rustradio::{Complex, Error};
use rustradio_ui::SharedVecPtr;

use crate::WorkerToMain;
use crate::worker::post_message;

/// A block that takes data from its input and posts it to the main UI thread.
///
/// The stream is identified by its name.
#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct ComplexSink {
    name: String,
    #[rustradio(in)]
    src: ReadStream<Complex>,
}

impl Block for ComplexSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        let (input, tags) = self.src.read_buf()?;
        let ilen = input.len();
        if ilen > 0 {
            post_message(WorkerToMain::SharedComplex(
                self.name.clone(),
                vec![SharedVecPtr::new(input.slice(), tags)],
            ))
            .map_err(|e| Error::msg(format!("post complex streams: {e:?}")))?;
            input.consume(ilen);
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}
