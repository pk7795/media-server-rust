//!
//! Channel Publisher will takecare of pubsub channel for sending data and handle when received channel feedback
//!

use std::{collections::VecDeque, fmt::Debug, hash::Hash};

use atm0s_sdn::features::pubsub::{self, ChannelControl, ChannelId, Feedback};
use indexmap::{IndexMap, IndexSet};
use media_server_protocol::{
    endpoint::{PeerId, TrackName},
    media::MediaPacket,
};
use media_server_utils::Count;
use sans_io_runtime::{return_if_err, return_if_none, TaskSwitcherChild};

use crate::{
    cluster::{id_generator, ClusterEndpointEvent, ClusterRemoteTrackEvent, ClusterRoomHash},
    transport::RemoteTrackId,
};

use super::Output;

pub enum FeedbackKind {
    Bitrate { min: u64, max: u64 },
    KeyFrameRequest,
}

impl TryFrom<Feedback> for FeedbackKind {
    type Error = ();
    fn try_from(value: Feedback) -> Result<Self, Self::Error> {
        match value.kind {
            0 => Ok(FeedbackKind::Bitrate { min: value.min, max: value.max }),
            1 => Ok(FeedbackKind::KeyFrameRequest),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub struct RoomChannelPublisher<Endpoint: Debug> {
    _c: Count<Self>,
    room: ClusterRoomHash,
    tracks: IndexMap<(Endpoint, RemoteTrackId), (PeerId, TrackName, ChannelId)>,
    tracks_source: IndexMap<ChannelId, IndexSet<(Endpoint, RemoteTrackId)>>, // We allow multi sources here for avoiding crash
    queue: VecDeque<Output<Endpoint>>,
}

impl<Endpoint: Debug + Hash + Eq + Copy> RoomChannelPublisher<Endpoint> {
    pub fn new(room: ClusterRoomHash) -> Self {
        Self {
            _c: Default::default(),
            room,
            tracks: Default::default(),
            tracks_source: Default::default(),
            queue: VecDeque::new(),
        }
    }

    pub fn on_track_feedback(&mut self, channel: ChannelId, fb: Feedback) {
        let fb = return_if_err!(FeedbackKind::try_from(fb));
        let sources = return_if_none!(self.tracks_source.get(&channel));
        for (endpoint, track_id) in sources {
            match fb {
                FeedbackKind::Bitrate { min, max } => {
                    log::debug!("[ClusterRoom {}/Publishers] channel {channel} limit bitrate [{min},{max}]", self.room);
                    self.queue.push_back(Output::Endpoint(
                        vec![*endpoint],
                        ClusterEndpointEvent::RemoteTrack(*track_id, ClusterRemoteTrackEvent::LimitBitrate { min, max }),
                    ));
                }
                FeedbackKind::KeyFrameRequest => {
                    log::debug!("[ClusterRoom {}/Publishers] channel {channel} request key_frame", self.room);
                    self.queue.push_back(Output::Endpoint(
                        vec![*endpoint],
                        ClusterEndpointEvent::RemoteTrack(*track_id, ClusterRemoteTrackEvent::RequestKeyFrame),
                    ));
                }
            }
        }
    }

    pub fn on_track_publish(&mut self, endpoint: Endpoint, track: RemoteTrackId, peer: PeerId, name: TrackName) {
        log::info!("[ClusterRoom {}/Publishers] peer ({peer} started track ({name})", self.room);
        let channel_id = id_generator::gen_track_channel_id(self.room, &peer, &name);
        self.tracks.insert((endpoint, track), (peer.clone(), name.clone(), channel_id));
        let sources = self.tracks_source.entry(channel_id).or_default();
        if sources.is_empty() {
            self.queue.push_back(Output::Pubsub(pubsub::Control(channel_id, ChannelControl::PubStart)));
        }
        sources.insert((endpoint, track));
    }

    pub fn on_track_data(&mut self, endpoint: Endpoint, track: RemoteTrackId, media: MediaPacket) {
        log::trace!(
            "[ClusterRoom {}/Publishers] peer {:?} track {track} publish media meta {:?} seq {}",
            self.room,
            endpoint,
            media.meta,
            media.seq
        );
        let (_peer, _name, channel_id) = return_if_none!(self.tracks.get(&(endpoint, track)));
        let data = media.serialize();
        self.queue.push_back(Output::Pubsub(pubsub::Control(*channel_id, ChannelControl::PubData(data))))
    }

    pub fn on_track_unpublish(&mut self, endpoint: Endpoint, track: RemoteTrackId) {
        let (peer, name, channel_id) = return_if_none!(self.tracks.swap_remove(&(endpoint, track)));
        let sources = self.tracks_source.get_mut(&channel_id).expect("Should have track_source");
        let removed = sources.swap_remove(&(endpoint, track));
        assert!(removed, "Should remove source child on unpublish");
        if sources.is_empty() {
            self.tracks_source.swap_remove(&channel_id).expect("Should remove source channel on unpublish");
            self.queue.push_back(Output::Pubsub(pubsub::Control(channel_id, ChannelControl::PubStop)));
        }
        log::info!("[ClusterRoom {}/Publishers] peer ({peer} stopped track {name})", self.room);
    }
}

impl<Endpoint: Debug + Hash + Eq + Copy> TaskSwitcherChild<Output<Endpoint>> for RoomChannelPublisher<Endpoint> {
    type Time = ();

    fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.tracks_source.is_empty() && self.queue.is_empty()
    }

    fn empty_event(&self) -> Output<Endpoint> {
        Output::OnResourceEmpty
    }

    fn pop_output(&mut self, _now: Self::Time) -> Option<Output<Endpoint>> {
        self.queue.pop_front()
    }
}

impl<Endpoint: Debug> Drop for RoomChannelPublisher<Endpoint> {
    fn drop(&mut self) {
        log::info!("[ClusterRoom {}/Publishers] Drop", self.room);
        assert_eq!(self.queue.len(), 0, "Queue not empty on drop {:?}", self.queue);
        assert_eq!(self.tracks.len(), 0, "Tracks not empty on drop {:?}", self.tracks);
        assert_eq!(self.tracks_source.len(), 0, "Tracks source not empty on drop {:?}", self.tracks_source);
    }
}

#[cfg(test)]
mod tests {
    use atm0s_sdn::features::pubsub::{ChannelControl, Control, Feedback};
    use media_server_protocol::{
        endpoint::{PeerId, TrackName},
        media::{MediaMeta, MediaPacket},
    };
    use sans_io_runtime::TaskSwitcherChild;

    use crate::{
        cluster::{ClusterEndpointEvent, ClusterRemoteTrackEvent},
        transport::RemoteTrackId,
    };

    use super::id_generator::gen_track_channel_id;
    use super::{super::Output, RoomChannelPublisher};

    pub fn fake_audio() -> MediaPacket {
        MediaPacket {
            ts: 0,
            seq: 0,
            marker: true,
            nackable: false,
            layers: None,
            meta: MediaMeta::Opus { audio_level: None },
            data: vec![1, 2, 3, 4],
        }
    }

    //Track start => should register with SDN
    //Track stop => should unregister with SDN
    //Track media => should send data over SDN
    #[test_log::test]
    fn channel_publish_data() {
        let room = 1.into();
        let mut publisher = RoomChannelPublisher::<u8>::new(room);

        let endpoint = 2;
        let track = RemoteTrackId::from(3);
        let peer = "peer1".to_string().into();
        let name = "audio_main".to_string().into();
        let channel_id = gen_track_channel_id(room, &peer, &name);
        publisher.on_track_publish(endpoint, track, peer, name);
        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubStart))));
        assert_eq!(publisher.pop_output(()), None);

        let media = fake_audio();
        publisher.on_track_data(endpoint, track, media.clone());
        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubData(media.serialize())))));
        assert_eq!(publisher.pop_output(()), None);

        publisher.on_track_unpublish(endpoint, track);
        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubStop))));
        assert_eq!(publisher.pop_output(()), None);
        assert!(publisher.is_empty());
    }

    //TODO Handle feedback: should handle KeyFrame feedback
    //TODO Handle feedback: should handle Bitrate feedback
    #[test_log::test]
    fn channel_feedback() {
        let room = 1.into();
        let mut publisher = RoomChannelPublisher::<u8>::new(room);

        let endpoint = 2;
        let track = RemoteTrackId::from(3);
        let peer = "peer1".to_string().into();
        let name = "audio_main".to_string().into();
        let channel_id = gen_track_channel_id(room, &peer, &name);
        publisher.on_track_publish(endpoint, track, peer, name);
        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubStart))));
        assert_eq!(publisher.pop_output(()), None);

        publisher.on_track_feedback(channel_id, Feedback::simple(0, 1000, 100, 200));
        assert_eq!(
            publisher.pop_output(()),
            Some(Output::Endpoint(
                vec![endpoint],
                ClusterEndpointEvent::RemoteTrack(track, ClusterRemoteTrackEvent::LimitBitrate { min: 1000, max: 1000 })
            ))
        );

        publisher.on_track_feedback(channel_id, Feedback::simple(1, 1, 100, 200));
        assert_eq!(
            publisher.pop_output(()),
            Some(Output::Endpoint(vec![endpoint], ClusterEndpointEvent::RemoteTrack(track, ClusterRemoteTrackEvent::RequestKeyFrame)))
        );
        assert_eq!(publisher.pop_output(()), None);

        publisher.on_track_unpublish(endpoint, track);
        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubStop))));
        assert_eq!(publisher.pop_output(()), None);
        assert!(publisher.is_empty());
    }

    #[test_log::test]
    fn two_sessions_same_room_peer_should_not_crash() {
        let room = 1.into();
        let mut publisher = RoomChannelPublisher::<u8>::new(room);

        let endpoint1 = 1;
        let endpoint2 = 2;
        let track = RemoteTrackId::from(3);
        let peer: PeerId = "peer1".to_string().into();
        let name: TrackName = "audio_main".to_string().into();
        let channel_id = gen_track_channel_id(room, &peer, &name);

        publisher.on_track_publish(endpoint1, track, peer.clone(), name.clone());
        publisher.on_track_publish(endpoint2, track, peer, name);

        assert!(publisher.pop_output(()).is_some()); // PubStart
        assert!(publisher.pop_output(()).is_none());

        publisher.on_track_unpublish(endpoint1, track);
        publisher.on_track_unpublish(endpoint2, track);

        assert_eq!(publisher.pop_output(()), Some(Output::Pubsub(Control(channel_id, ChannelControl::PubStop)))); // PubStop
        assert_eq!(publisher.pop_output(()), None);
        assert!(publisher.is_empty());
    }
}
