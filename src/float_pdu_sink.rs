//! A sink block that posts PDUs of float from worker to main UI thread.
use rustradio::block::{Block, BlockRet};
use rustradio::stream::NCReadStream;
use rustradio::{Error, Float};

use crate::FloatPduStream;
use crate::worker::post_message;

type WorkerToMain = crate::WorkerToMain<crate::AppEmpty>;

const DEBUG_KEEP_1_IN_N: usize = 1;

/// A block that takes a PDU full of floats and posts it to the main UI thread.
///
/// The stream is identified by its name.
#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct FloatPduSink {
    name: String,
    sample_rate: Float,
    #[rustradio(in)]
    src: NCReadStream<Vec<Float>>,

    // This is used for debugging only.
    #[rustradio(default)]
    skip: usize,
}

impl Block for FloatPduSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        loop {
            let Some((samples, _tags)) = self.src.pop() else {
                return Ok(BlockRet::WaitForStream(&self.src, 1));
            };
            self.skip += 1;
            if self.skip == DEBUG_KEEP_1_IN_N {
                post_message(&WorkerToMain::FloatPduStreams(vec![FloatPduStream {
                    name: self.name.clone(),
                    sample_rate: self.sample_rate,
                    samples,
                }]))
                .map_err(|e| Error::msg(format!("post float PDU stream: {e:?}")))?;
                self.skip = 0;
            }
        }
    }
}
