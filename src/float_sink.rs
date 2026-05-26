use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, Tag};
use rustradio::{Error, Float};
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "type", content = "data")]
enum BorrowedWorkerToMain<'a> {
    FloatStreams([BorrowedFloatStream<'a>; 1]),
}

#[derive(Serialize)]
struct BorrowedFloatStream<'a> {
    name: &'a str,
    tags: Vec<Tag>,
    samples: &'a [Float],
}

#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct FloatSink {
    name: String,
    #[rustradio(in)]
    src: ReadStream<Float>,
}

impl FloatSink {
    fn post_snapshot(&self, samples: &[Float], tags: Vec<Tag>) -> rustradio::Result<()> {
        crate::worker::post_message(&BorrowedWorkerToMain::FloatStreams([BorrowedFloatStream {
            name: &self.name,
            tags,
            samples,
        }]))
        .map_err(|e| Error::msg(format!("post float streams: {e:?}")))?;
        Ok(())
    }
}

impl Block for FloatSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        let (input, tags) = self.src.read_buf()?;
        let ilen = input.len();
        if ilen > 0 {
            self.post_snapshot(input.slice(), tags)?;
            input.consume(ilen);
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}
