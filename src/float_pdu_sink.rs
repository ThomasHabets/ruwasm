//! A sink block that posts PDUs of float from worker to main UI thread.
use rustradio::Float;
use rustradio::block::{Block, BlockRet};
use rustradio::stream::{NCReadStream, Tag, TagValue};
use rustradio_ui::TaggedVec;

use crate::WorkerToMain;
use crate::worker::send_message_from_sync;

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
            let Some((samples, mut tags)) = self.src.pop() else {
                return Ok(BlockRet::WaitForStream(&self.src, 1));
            };
            self.skip += 1;
            if self.skip == DEBUG_KEEP_1_IN_N {
                tags.push(Tag::new(
                    0,
                    "rustradio::sample_rate",
                    TagValue::Float(self.sample_rate),
                ));
                send_message_from_sync(WorkerToMain::Floats(
                    self.name.clone(),
                    vec![TaggedVec {
                        data: samples,
                        tags,
                    }],
                ));
                self.skip = 0;
            }
        }
    }
}
