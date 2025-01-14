//!
//! Cluster handle all of logic allow multi node can collaborate to make a giant streaming system.
//!
//! Cluster is collect of some rooms, each room is independent logic.
//! We use UserData feature from SDN with UserData is RoomUserData to route SDN event to correct room.
//!

use derive_more::{AsRef, Display, From};
use indexmap::IndexMap;
use sans_io_runtime::{return_if_none, TaskGroup, TaskGroupOutput, TaskSwitcherChild};
use std::{
    fmt::Debug,
    hash::{Hash, Hasher},
    time::Instant,
};

use atm0s_sdn::features::{FeaturesControl, FeaturesEvent};
use media_server_protocol::{
    endpoint::{AudioMixerConfig, PeerId, PeerMeta, RoomId, RoomInfoPublish, RoomInfoSubscribe, TrackMeta, TrackName, TrackSource},
    media::MediaPacket,
    multi_tenancy::AppContext,
};

use crate::{
    endpoint::MessageChannelLabel,
    transport::{LocalTrackId, RemoteTrackId},
};

use self::room::ClusterRoom;
pub use self::room::RoomUserData;

mod id_generator;
mod room;

#[derive(Clone, Copy, From, AsRef, PartialEq, Eq, Debug, Display, Hash)]
pub struct ClusterRoomHash(u64);

impl ClusterRoomHash {
    pub fn generate(app: &AppContext, room: &RoomId) -> Self {
        let mut hash = std::hash::DefaultHasher::new();
        app.app.hash(&mut hash);
        room.as_ref().hash(&mut hash);
        Self(hash.finish())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterRemoteTrackControl {
    Started(TrackName, TrackMeta),
    Media(MediaPacket),
    Ended(TrackName, TrackMeta),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterRemoteTrackEvent {
    RequestKeyFrame,
    LimitBitrate { min: u64, max: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterLocalTrackControl {
    Subscribe(PeerId, TrackName),
    RequestKeyFrame,
    DesiredBitrate(u64),
    Unsubscribe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterLocalTrackEvent {
    RelayChanged,
    SourceChanged,
    Media(u64, MediaPacket),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterAudioMixerControl {
    Attach(Vec<TrackSource>),
    Detach(Vec<TrackSource>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterAudioMixerEvent {
    SlotSet(u8, PeerId, TrackName),
    SlotUnset(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterMessageChannelControl {
    StartPublish,
    StopPublish,
    Subscribe,
    Unsubscribe,
    PublishData(PeerId, Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClusterEndpointControl {
    Join(PeerId, PeerMeta, RoomInfoPublish, RoomInfoSubscribe, Option<AudioMixerConfig>),
    Leave,
    SubscribePeer(PeerId),
    UnsubscribePeer(PeerId),
    AudioMixer(ClusterAudioMixerControl),
    RemoteTrack(RemoteTrackId, ClusterRemoteTrackControl),
    LocalTrack(LocalTrackId, ClusterLocalTrackControl),
    MessageChannel(MessageChannelLabel, ClusterMessageChannelControl),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterEndpointEvent {
    PeerJoined(PeerId, PeerMeta),
    PeerLeaved(PeerId, PeerMeta),
    TrackStarted(PeerId, TrackName, TrackMeta),
    TrackStopped(PeerId, TrackName, TrackMeta),
    AudioMixer(ClusterAudioMixerEvent),
    RemoteTrack(RemoteTrackId, ClusterRemoteTrackEvent),
    LocalTrack(LocalTrackId, ClusterLocalTrackEvent),
    MessageChannelData(MessageChannelLabel, PeerId, Vec<u8>),
}

pub enum Input<Endpoint> {
    Sdn(ClusterRoomHash, FeaturesEvent),
    Endpoint(Endpoint, ClusterRoomHash, ClusterEndpointControl),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Output<Endpoint> {
    Sdn(RoomUserData, FeaturesControl),
    Endpoint(Vec<Endpoint>, ClusterEndpointEvent),
    OnResourceEmpty,
    Continue,
}

pub struct MediaCluster<Endpoint: Debug + Copy + Clone + Hash + Eq> {
    rooms_map: IndexMap<ClusterRoomHash, usize>,
    rooms: TaskGroup<room::Input<Endpoint>, room::Output<Endpoint>, ClusterRoom<Endpoint>, 16>,
    shutdown: bool,
}

impl<Endpoint: Debug + Copy + Hash + Eq + Clone> Default for MediaCluster<Endpoint> {
    fn default() -> Self {
        Self {
            rooms_map: IndexMap::new(),
            rooms: TaskGroup::default(),
            shutdown: false,
        }
    }
}

impl<Endpoint: Debug + Hash + Copy + Clone + Debug + Eq> MediaCluster<Endpoint> {
    pub fn on_tick(&mut self, now: Instant) {
        self.rooms.on_tick(now);
    }

    pub fn on_sdn_event(&mut self, now: Instant, userdata: RoomUserData, event: FeaturesEvent) {
        let index = return_if_none!(self.rooms_map.get(&userdata.0));
        self.rooms.on_event(now, *index, room::Input::Sdn(userdata, event));
    }

    pub fn on_endpoint_control(&mut self, now: Instant, endpoint: Endpoint, room_hash: ClusterRoomHash, control: ClusterEndpointControl) {
        if let Some(index) = self.rooms_map.get(&room_hash) {
            self.rooms.on_event(now, *index, room::Input::Endpoint(endpoint, control));
        } else {
            log::info!("[MediaCluster] create room {}", room_hash);
            let index = self.rooms.add_task(ClusterRoom::new(room_hash));
            self.rooms_map.insert(room_hash, index);
            self.rooms.on_event(now, index, room::Input::Endpoint(endpoint, control));
        }
    }

    pub fn shutdown(&mut self, now: Instant) {
        if self.shutdown {
            return;
        }
        self.rooms.on_shutdown(now);
        self.shutdown = true;
    }
}

impl<Endpoint: Debug + Hash + Copy + Clone + Debug + Eq> TaskSwitcherChild<Output<Endpoint>> for MediaCluster<Endpoint> {
    type Time = ();

    fn is_empty(&self) -> bool {
        self.shutdown && self.rooms.is_empty()
    }

    fn empty_event(&self) -> Output<Endpoint> {
        Output::OnResourceEmpty
    }

    fn pop_output(&mut self, _now: Self::Time) -> Option<Output<Endpoint>> {
        let (index, out) = match self.rooms.pop_output(())? {
            TaskGroupOutput::TaskOutput(index, out) => (index, out),
            TaskGroupOutput::OnResourceEmpty => return Some(Output::Continue),
        };
        match out {
            room::Output::Sdn(userdata, control) => Some(Output::Sdn(userdata, control)),
            room::Output::Endpoint(endpoints, event) => Some(Output::Endpoint(endpoints, event)),
            room::Output::OnResourceEmpty(room) => {
                log::info!("[MediaCluster] remove room index {index}, hash {room}");
                self.rooms_map.swap_remove(&room).expect("Should have room with index");
                self.rooms.remove_task(index);
                Some(Output::Continue)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use atm0s_sdn::features::{
        dht_kv::{self, MapControl, MapEvent},
        FeaturesControl, FeaturesEvent,
    };
    use media_server_protocol::{
        endpoint::{PeerId, PeerInfo, PeerMeta, RoomId, RoomInfoPublish, RoomInfoSubscribe},
        multi_tenancy::{AppContext, AppId},
    };
    use sans_io_runtime::TaskSwitcherChild;

    use crate::cluster::{
        id_generator,
        room::{RoomFeature, RoomUserData},
        ClusterEndpointEvent,
    };

    use super::{ClusterEndpointControl, ClusterRoomHash, MediaCluster, Output};

    #[test_log::test]
    fn multi_tenancy_room() {
        let app_root = AppContext { app: AppId::root_app() };
        let app1 = AppContext { app: AppId::from("app1") };
        let app2 = AppContext { app: AppId::from("app2") };

        let room: RoomId = RoomId::from("same_room");

        let app_root_room = ClusterRoomHash::generate(&app_root, &room);
        let app1_room = ClusterRoomHash::generate(&app1, &room);
        let app2_room = ClusterRoomHash::generate(&app2, &room);

        assert_ne!(app_root_room, app1_room);
        assert_ne!(app_root_room, app2_room);
        assert_ne!(app1_room, app2_room);
    }

    //TODO should create room when new room event arrived
    //TODO should route to correct room
    //TODO should remove room after all peers leaved
    #[test_log::test]
    fn room_manager_should_work() {
        let mut cluster = MediaCluster::<u8>::default();

        let endpoint = 1;
        let userdata = RoomUserData(ClusterRoomHash(1), RoomFeature::MetaData);
        let room_peers_map = id_generator::peers_map(userdata.0);
        let peer = PeerId::from("peer1");
        let peer_key = id_generator::peers_key(&peer);
        let peer_info = PeerInfo::new(peer.clone(), PeerMeta { metadata: None, extra_data: None });

        let now = Instant::now();
        // Not join room with scope (peer true, track false) should Set and Sub
        cluster.on_endpoint_control(
            now,
            endpoint,
            userdata.0,
            ClusterEndpointControl::Join(
                peer.clone(),
                peer_info.meta.clone(),
                RoomInfoPublish { peer: true, tracks: false },
                RoomInfoSubscribe { peers: true, tracks: false },
                None,
            ),
        );
        assert_eq!(
            cluster.pop_output(()),
            Some(Output::Sdn(
                userdata,
                FeaturesControl::DhtKv(dht_kv::Control::MapCmd(room_peers_map, MapControl::Set(peer_key, peer_info.serialize())))
            ))
        );
        assert_eq!(
            cluster.pop_output(()),
            Some(Output::Sdn(userdata, FeaturesControl::DhtKv(dht_kv::Control::MapCmd(room_peers_map, MapControl::Sub))))
        );
        assert_eq!(cluster.pop_output(()), None);
        assert_eq!(cluster.rooms.tasks(), 1);
        assert_eq!(cluster.rooms_map.len(), 1);

        // Correct forward to room
        cluster.on_sdn_event(
            now,
            userdata,
            FeaturesEvent::DhtKv(dht_kv::Event::MapEvent(room_peers_map, MapEvent::OnSet(peer_key, 1, peer_info.serialize()))),
        );
        assert_eq!(
            cluster.pop_output(()),
            Some(Output::Endpoint(vec![endpoint], ClusterEndpointEvent::PeerJoined(peer.clone(), peer_info.meta.clone())))
        );
        assert_eq!(cluster.pop_output(()), None);

        // Now leave room should Del and Unsub
        cluster.on_endpoint_control(now, endpoint, userdata.0, ClusterEndpointControl::Leave);
        assert_eq!(
            cluster.pop_output(()),
            Some(Output::Sdn(userdata, FeaturesControl::DhtKv(dht_kv::Control::MapCmd(room_peers_map, MapControl::Del(peer_key)))))
        );
        assert_eq!(
            cluster.pop_output(()),
            Some(Output::Sdn(userdata, FeaturesControl::DhtKv(dht_kv::Control::MapCmd(room_peers_map, MapControl::Unsub))))
        );
        assert_eq!(cluster.pop_output(()), Some(Output::Continue)); //this is for destroy event
        assert_eq!(cluster.pop_output(()), None);
        assert_eq!(cluster.rooms.tasks(), 0);
        assert_eq!(cluster.rooms_map.len(), 0);
    }
}
