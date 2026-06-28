//! A sink block that posts the data stream from worker to main UI thread.
use rustradio::Float;
use rustradio::block::{Block, BlockRet};
use rustradio::stream::ReadStream;
use rustradio_ui::TaggedVec;

use crate::worker::send_message_from_sync;

//type WorkerToMain = rustradio_ui::WorkerToMain<rustradio_ui::AppEmpty>;
use crate::WorkerToMain;

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
            send_message_from_sync(WorkerToMain::Floats(
                self.name.clone(),
                vec![TaggedVec {
                    data: input.slice().to_vec(),
                    tags,
                }],
            ));
            input.consume(ilen);
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}
