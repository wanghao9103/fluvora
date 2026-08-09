//! RTCP compound-packet codec for sender/receiver reports and WebRTC feedback.

mod codec;
mod error;
mod model;
mod twcc;

pub use codec::{encode_compound, parse_compound};
pub use error::RtcpError;
pub use model::{
    GenericNack, NackEntry, Packet, PictureLossIndication, RawPacket, ReceiverReport, ReportBlock,
    SdesChunk, SdesItem, SenderReport, SourceDescription, TransportWideFeedback, TwccStatus,
};
