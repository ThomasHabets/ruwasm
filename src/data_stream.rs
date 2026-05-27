//! Worker-side DATA_STREAM protocol state.
//!
//! This keeps the protocol handshake and per-stream request bookkeeping out of
//! the worker graph wiring.

use std::collections::HashMap;

use rustradio::data_stream::{BytesReader, DataStreamId, PROTOCOL_VERSION, Packet, SyncWriter};
use rustradio::{Error, Result};

/// Per-receiver flow-control state tracked by the worker.
struct StreamState {
    outstanding_req: bool,
}

/// Protocol events emitted after parsing bytes from the selected data supplier.
pub(crate) enum Event {
    /// The peer sent a supported DATA_STREAM Version packet.
    PeerReady,
    /// Bytes arrived for one registered receiver.
    Data {
        receiver: DataStreamId,
        data: Vec<u8>,
    },
    /// The peer ended one receiver by sending an empty Data packet.
    Eof { receiver: DataStreamId },
}

/// DATA_STREAM protocol coordinator for worker-side source receivers.
pub(crate) struct DataStream {
    reader: BytesReader,
    version_sent: bool,
    peer_version_seen: bool,
    streams: HashMap<DataStreamId, StreamState>,
}

impl DataStream {
    /// Create a fresh protocol state machine with no registered receivers.
    pub(crate) fn new() -> Self {
        Self {
            reader: BytesReader::new(),
            version_sent: false,
            peer_version_seen: false,
            streams: HashMap::new(),
        }
    }

    /// Register a receiver that may request bytes from the peer.
    pub(crate) fn register_receiver(&mut self, receiver: DataStreamId) {
        self.streams.entry(receiver).or_insert(StreamState {
            outstanding_req: false,
        });
    }

    /// Remove a receiver after EOF or graph teardown.
    pub(crate) fn disable_receiver(&mut self, receiver: &DataStreamId) {
        self.streams.remove(receiver);
        if self.streams.is_empty() {
            self.reset_protocol();
        }
    }

    /// Mark the transport as disconnected and return receivers to EOF.
    pub(crate) fn disconnect(&mut self) -> Vec<DataStreamId> {
        let receivers = self.receivers();
        self.streams.clear();
        self.reset_protocol();
        receivers
    }

    /// Return the receivers currently participating in this protocol session.
    pub(crate) fn receivers(&self) -> Vec<DataStreamId> {
        self.streams.keys().cloned().collect()
    }

    /// Build DATA_STREAM bytes for a source request if the protocol allows it.
    pub(crate) fn request_data(
        &mut self,
        receiver: &DataStreamId,
        window: usize,
    ) -> Result<RequestState> {
        let mut packet = Vec::new();
        if !self.version_sent {
            SyncWriter::new(&mut packet).write_version()?;
            self.version_sent = true;
        }

        let stream = self.streams.entry(receiver.clone()).or_insert(StreamState {
            outstanding_req: false,
        });

        if stream.outstanding_req {
            return if packet.is_empty() {
                Ok(RequestState::Waiting)
            } else {
                Ok(RequestState::Issued(packet))
            };
        }

        /*
        if !self.peer_version_seen {
            return if packet.is_empty() {
                Ok(RequestState::NotReady)
            } else {
                Ok(RequestState::Issued(packet))
            };
        }
        */

        SyncWriter::new(&mut packet).write_request_data(receiver, window)?;
        stream.outstanding_req = true;
        Ok(RequestState::Issued(packet))
    }

    /// Parse incoming protocol bytes and return the resulting worker events.
    pub(crate) fn handle_bytes(&mut self, data: &[u8]) -> Result<Vec<Event>> {
        self.reader.push_bytes(data);
        let mut events = Vec::new();

        loop {
            let Some(packet) = self.reader.read_packet()? else {
                break;
            };
            if let Some(event) = self.handle_packet(packet)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    /// Apply one decoded packet to the protocol state machine.
    fn handle_packet(&mut self, packet: Packet) -> Result<Option<Event>> {
        match packet {
            Packet::Version(PROTOCOL_VERSION) => {
                log::info!("DATA_STREAM peer version accepted");
                self.peer_version_seen = true;
                Ok(Some(Event::PeerReady))
            }
            Packet::Version(version) => Err(Error::msg(format!(
                "unsupported DATA_STREAM protocol version {version}"
            ))),
            Packet::Data(data) => {
                if !self.peer_version_seen {
                    return Err(Error::msg("peer sent DATA_STREAM Data before Version"));
                }
                let receiver = data.stream_id;
                let bytes = data.data;
                if let Some(stream) = self.streams.get_mut(&receiver) {
                    stream.outstanding_req = false;
                }
                if bytes.is_empty() {
                    self.disable_receiver(&receiver);
                    Ok(Some(Event::Eof { receiver }))
                } else {
                    Ok(Some(Event::Data {
                        receiver,
                        data: bytes,
                    }))
                }
            }
            Packet::RequestData(req) => {
                if !self.peer_version_seen {
                    return Err(Error::msg(
                        "peer sent DATA_STREAM RequestData before Version",
                    ));
                }
                Err(Error::msg(format!(
                    "unexpected DATA_STREAM RequestData packet for {} with window {}",
                    req.stream_id, req.window
                )))
            }
        }
    }

    /// Clear session-local protocol state after all receivers are gone.
    fn reset_protocol(&mut self) {
        self.reader.clear();
        self.version_sent = false;
        self.peer_version_seen = false;
    }
}

/// Result of asking the protocol coordinator for more source data.
pub(crate) enum RequestState {
    // This should not be needed. We don't care if remote sent a Version before
    // we send ReqData. We assume that the remote end won't accept anything with
    // the wrong version, is all.
    //
    // The worker is still waiting for the peer Version packet.
    // NotReady,
    /// A request is already outstanding for this receiver.
    Waiting,
    /// Protocol bytes should be sent to the selected data supplier.
    Issued(Vec<u8>),
}
