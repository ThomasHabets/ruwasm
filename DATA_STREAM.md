# Data stream thoughts

It looks like there's no built in flow control in websockets. We therefore need
to build some sort of windowing ourselves on the communication from websocket,
to main thread, to worker.

## Requirements

* Performant. Probably want to provide a "receiver window" kind of thing, with
  periodic updates.
* Multi-stream. The WASM may only be a UI for a whole flowgraph that runs
  native.
* Bidirectional. Browser audio or whatever, could be required to feed back.
* Support "messages" too, for when the UI needs to tell the websocket server to
  change frequency.
