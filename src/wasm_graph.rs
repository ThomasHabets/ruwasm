use log::info;

//use futures::channel::mpsc;
use futures::StreamExt;
use futures_channel::mpsc;
//use wasmer_types::lib::std::sync::mpsc;
//use tokio::sync::mpsc;
use rustradio::block::{Block, BlockRet};
use rustradio::graph::{CancellationToken, GraphRunner};

pub struct WasmGraph {
    blocks: Vec<Box<dyn Block>>,
}

impl WasmGraph {
    pub fn new() -> Self {
        Self { blocks: vec![] }
    }
    pub async fn run_async(&mut self, rx: async_channel::Receiver<()>) -> rustradio::Result<()> {
        let mut eof = vec![false; self.blocks.len()];
        let rx = Box::pin(rx);
        loop {
            let mut done = true;
            let mut need_more = false;
            for (n, b) in self.blocks.iter_mut().enumerate() {
                let name = b.block_name().to_owned();
                if eof[n] {
                    continue;
                }
                let ret = b.work()?;
                match ret {
                    BlockRet::EOF => {
                        eof[n] = true;
                        info!("Block({name}): EOF");
                    }
                    BlockRet::Again => done = false,
                    // TODO: Skip calling next time if conditions not met?
                    BlockRet::WaitForStream(s, _) => {
                        let closed = s.closed();
                        drop(ret);
                        if b.eof() && closed {
                            eof[n] = true;
                        }
                    }
                    BlockRet::WaitForFunc(_) => {}
                    BlockRet::Pending => {
                        //info!("Block {name} returned Pending");
                        need_more = true;
                        done = false;
                    }
                }
            }
            if done {
                info!("Wasm graph: All done");
                return Ok(());
            }
            if need_more {
                //info!("Graph: About to wait for more somethings");
                if let Err(e) = rx.recv().await {
                    info!("Graph: recv error: {e:?}");
                    // This can only happen if the sender crashed. If the worker
                    // crashed, then there's no point in continuing the graph
                    // connected to nothing.
                    return Err(rustradio::Error::msg("recv()"));
                }
            }
        }
    }
}

impl GraphRunner for WasmGraph {
    fn add(&mut self, b: Box<dyn Block + Send>) {
        self.blocks.push(b);
    }
    fn run(&mut self) -> rustradio::Result<()> {
        todo!()
    }
    fn generate_stats(&self) -> Option<String> {
        None
    }
    fn cancel_token(&self) -> CancellationToken {
        todo!()
    }
}
