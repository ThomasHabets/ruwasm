use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, Tag};
use rustradio::{Complex, Error};
use serde::Serialize;

#[derive(rustradio_macros::Block)]
#[rustradio(new)]
pub struct ComplexSink {
    name: String,
    #[rustradio(in)]
    src: ReadStream<Complex>,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "data")]
enum BorrowedWorkerToMain<'a> {
    ComplexStreams([BorrowedComplexStream<'a>; 1]),
}

#[derive(Serialize)]
struct BorrowedComplexStream<'a> {
    name: &'a str,
    tags: Vec<Tag>,
    samples: &'a [Complex],
}

impl ComplexSink {
    fn post_snapshot(&self, samples: &[Complex], tags: Vec<Tag>) -> rustradio::Result<()> {
        crate::worker::post_message(&BorrowedWorkerToMain::ComplexStreams([
            BorrowedComplexStream {
                name: &self.name,
                tags,
                samples,
            },
        ]))
        .map_err(|e| Error::msg(format!("post complex streams: {e:?}")))?;
        Ok(())
    }
}

impl Block for ComplexSink {
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
