use rustradio::block::{Block, BlockRet};
use rustradio::stream::NCReadStream;
use rustradio::{Error, Float};

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
        crate::worker::post_message(&WorkerToMain::FloatPduStreams(vec![FloatPduStream {
            name: self.name.clone(),
            sample_rate: self.sample_rate,
            samples,
        }]))
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
