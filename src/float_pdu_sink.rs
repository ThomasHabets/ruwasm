use rustradio::block::{Block, BlockRet};
use rustradio::stream::NCReadStream;
use rustradio::{Error, Float};
use wasm_bindgen::JsCast;

use crate::{FloatPduStream, WorkerToMain};

#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct FloatPduSink {
    name: String,
    sample_rate: Float,
    #[rustradio(in)]
    src: NCReadStream<Vec<Float>>,
}

impl FloatPduSink {
    fn post_frame(&self, samples: Vec<Float>) -> rustradio::Result<()> {
        let scope = web_sys::js_sys::global()
            .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
            .map_err(|e| Error::msg(format!("not in worker scope: {e:?}")))?;
        scope
            .post_message(
                &WorkerToMain::FloatPduStreams(vec![FloatPduStream {
                    name: self.name.clone(),
                    sample_rate: self.sample_rate,
                    samples,
                }])
                .try_into()
                .map_err(|e| Error::msg(format!("serialize float PDU stream: {e:?}")))?,
            )
            .map_err(|e| Error::msg(format!("post float PDU stream: {e:?}")))?;
        Ok(())
    }
}

impl Block for FloatPduSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        let Some((samples, _tags)) = self.src.pop() else {
            return Ok(BlockRet::WaitForStream(&self.src, 1));
        };
        self.post_frame(samples)?;
        Ok(BlockRet::Again)
    }
}
