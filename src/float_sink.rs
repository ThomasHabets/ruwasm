use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, Tag};
/// TODO: send what we're holding on eof.
use rustradio::{Error, Float};
use wasm_bindgen::JsCast;

use crate::{FloatStream, WorkerToMain};

#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct FloatSink {
    name: String,
    #[rustradio(in)]
    src: ReadStream<Float>,
    #[rustradio(default)]
    samples: Vec<Float>,
    #[rustradio(default)]
    tags: Vec<Tag>,
}

impl FloatSink {
    fn post_snapshot(&self) -> rustradio::Result<()> {
        let scope = web_sys::js_sys::global()
            .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
            .map_err(|e| Error::msg(format!("not in worker scope: {e:?}")))?;
        scope
            .post_message(
                &WorkerToMain::FloatStreams(vec![FloatStream {
                    name: self.name.clone(),
                    tags: self.tags.clone(),
                    samples: self.samples.clone(),
                }])
                .try_into()
                .map_err(|e| Error::msg(format!("serialize float streams: {e:?}")))?,
            )
            .map_err(|e| Error::msg(format!("post float streams: {e:?}")))?;
        Ok(())
    }
}

impl Block for FloatSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        let (input, tags) = self.src.read_buf()?;
        let ilen = input.len();
        if ilen > 0 {
            self.samples.extend_from_slice(input.slice());
            self.tags.extend(tags);
            input.consume(ilen);
            self.post_snapshot()?;
            self.samples.clear();
            self.tags.clear();
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}
