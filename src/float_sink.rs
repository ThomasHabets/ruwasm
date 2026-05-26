//! A sink block that posts the data stream from worker to main UI thread.
use rustradio::block::{Block, BlockRet};
use rustradio::stream::ReadStream;
use rustradio::{Error, Float};

use crate::FloatStreamRef;
use crate::WorkerToMainRef;
use crate::worker::post_message;

/// A block that takes float data from its input and posts it to the main UI
/// thread.
///
/// The stream is identified by its name.
#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct FloatSink {
    name: String,
    #[rustradio(in)]
    src: ReadStream<Float>,
}

impl Block for FloatSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        let (input, tags) = self.src.read_buf()?;
        let ilen = input.len();
        if ilen > 0 {
            post_message(&WorkerToMainRef::FloatStreams(vec![FloatStreamRef {
                name: &self.name,
                tags,
                samples: input.slice(),
            }]))
            .map_err(|e| Error::msg(format!("post float streams: {e:?}")))?;
            input.consume(ilen);
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}
