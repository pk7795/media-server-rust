use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use media_server_core::{
    cluster::{ClusterEndpointControl, ClusterEndpointEvent, ClusterRoomHash},
    endpoint::{Endpoint, EndpointCfg, EndpointInput, EndpointOutput},
};
use media_server_protocol::{
    cluster::gen_cluster_session_id,
    multi_tenancy::{AppContext, AppId},
    protobuf::cluster_connector::peer_event,
    record::SessionRecordEvent,
    transport::{RpcError, RpcResult},
};
use media_server_secure::MediaEdgeSecure;
use sans_io_runtime::{
    backend::{BackendIncoming, BackendOutgoing},
    group_owner_type, return_if_none, return_if_some, TaskGroup, TaskGroupOutput, TaskSwitcherChild,
};
use str0m::change::DtlsCert;

use crate::{
    shared_port::SharedUdpPort,
    transport::{ExtIn, ExtOut, TransportWebrtc, VariantParams},
    WebrtcError,
};

group_owner_type!(WebrtcSession);

#[allow(clippy::large_enum_variant)]
pub enum GroupInput {
    Net(BackendIncoming),
    Cluster(WebrtcSession, ClusterEndpointEvent),
    Ext(WebrtcSession, ExtIn),
}

#[derive(Debug)]
pub enum GroupOutput {
    Net(BackendOutgoing),
    Cluster(WebrtcSession, ClusterRoomHash, ClusterEndpointControl),
    PeerEvent(WebrtcSession, AppId, u64, Instant, peer_event::Event),
    RecordEvent(WebrtcSession, u64, Instant, SessionRecordEvent),
    Ext(WebrtcSession, ExtOut),
    OnResourceEmpty,
    Continue,
}

#[allow(clippy::type_complexity)]
pub struct MediaWorkerWebrtc<ES: 'static + MediaEdgeSecure> {
    ice_lite: bool,
    addrs_alt: Vec<SocketAddr>,
    shared_port: SharedUdpPort<usize>,
    dtls_cert: DtlsCert,
    endpoints: TaskGroup<EndpointInput<ExtIn>, EndpointOutput<ExtOut>, Endpoint<TransportWebrtc<ES>, ExtIn, ExtOut>, 16>,
    addrs: Vec<(SocketAddr, usize)>,
    queue: VecDeque<GroupOutput>,
    secure: Arc<ES>,
    shutdown: bool,
}

impl<ES: MediaEdgeSecure> MediaWorkerWebrtc<ES> {
    pub fn new(addrs: Vec<SocketAddr>, addrs_alt: Vec<SocketAddr>, ice_lite: bool, secure: Arc<ES>) -> Self {
        Self {
            ice_lite,
            addrs_alt,
            shared_port: SharedUdpPort::default(),
            dtls_cert: DtlsCert::new_openssl(),
            endpoints: TaskGroup::default(),
            addrs: vec![],
            queue: VecDeque::from(addrs.iter().map(|addr| GroupOutput::Net(BackendOutgoing::UdpListen { addr: *addr, reuse: false })).collect::<Vec<_>>()),
            secure,
            shutdown: false,
        }
    }

    pub fn spawn(&mut self, app: AppContext, remote: IpAddr, session_id: u64, variant: VariantParams<ES>, offer: &str) -> RpcResult<(bool, String, usize)> {
        let cfg = match &variant {
            VariantParams::Whip(_, _, _, record) => EndpointCfg {
                app: app.clone(),
                max_ingress_bitrate: 2_500_000,
                max_egress_bitrate: 2_500_000,
                record: *record,
            },
            VariantParams::Whep(_, _, _) => EndpointCfg {
                app: app.clone(),
                max_ingress_bitrate: 2_500_000,
                max_egress_bitrate: 2_500_000,
                record: false,
            },
            VariantParams::Webrtc(_, _, _, record, _) => EndpointCfg {
                app: app.clone(),
                max_ingress_bitrate: 2_500_000,
                max_egress_bitrate: 2_500_000,
                record: *record,
            },
        };
        let (tran, ufrag, sdp) = TransportWebrtc::new(app, remote, variant, offer, self.dtls_cert.clone(), &self.addrs, &self.addrs_alt, self.ice_lite)?;
        log::info!("[TransportWebrtc] create endpoint with config {:?}", cfg);
        let endpoint = Endpoint::new(session_id, cfg, tran);
        let index = self.endpoints.add_task(endpoint);
        self.shared_port.add_ufrag(ufrag, index);
        Ok((self.ice_lite, sdp, index))
    }

    fn process_output(&mut self, index: usize, out: EndpointOutput<ExtOut>) -> GroupOutput {
        match out {
            EndpointOutput::Net(net) => GroupOutput::Net(net),
            EndpointOutput::Cluster(room, control) => GroupOutput::Cluster(WebrtcSession(index), room, control),
            EndpointOutput::PeerEvent(app, session_id, ts, event) => GroupOutput::PeerEvent(WebrtcSession(index), app, session_id, ts, event),
            EndpointOutput::RecordEvent(session_id, ts, event) => GroupOutput::RecordEvent(WebrtcSession(index), session_id, ts, event),
            EndpointOutput::OnResourceEmpty => {
                log::info!("[TransportWebrtc] destroy endpoint {index}");
                self.endpoints.remove_task(index);
                self.shared_port.remove_task(index);
                GroupOutput::Continue
            }
            EndpointOutput::Ext(ext) => GroupOutput::Ext(WebrtcSession(index), ext),
            EndpointOutput::Continue => GroupOutput::Continue,
        }
    }
}

impl<ES: MediaEdgeSecure> MediaWorkerWebrtc<ES> {
    pub fn tasks(&self) -> usize {
        self.endpoints.tasks()
    }

    pub fn on_tick(&mut self, now: Instant) {
        self.endpoints.on_tick(now);
    }

    pub fn on_event(&mut self, now: Instant, input: GroupInput) {
        match input {
            GroupInput::Net(BackendIncoming::UdpListenResult { bind, result }) => {
                if let Ok((addr, slot)) = result {
                    log::info!("[MediaWorkerWebrtc] successful bind udp port {addr}, slot {slot}");
                    self.addrs.push((addr, slot));
                } else {
                    log::warn!("[MediaWorkerWebrtc] unsuccessful bind {bind}");
                }
            }
            GroupInput::Net(BackendIncoming::UdpPacket { slot, from, data }) => {
                let index = return_if_none!(self.shared_port.map_remote(from, &data));
                self.endpoints.on_event(now, index, EndpointInput::Net(BackendIncoming::UdpPacket { slot, from, data }));
            }
            GroupInput::Cluster(owner, event) => {
                self.endpoints.on_event(now, owner.index(), EndpointInput::Cluster(event));
            }
            GroupInput::Ext(owner, ext) => {
                log::info!("[MediaWorkerWebrtc] on ext to owner {:?}", owner);
                if self.endpoints.has_task(owner.index()) {
                    self.endpoints.on_event(now, owner.index(), EndpointInput::Ext(ext));
                } else {
                    match ext {
                        ExtIn::RemoteIce(req_id, variant, ..) => {
                            self.queue
                                .push_back(GroupOutput::Ext(owner, ExtOut::RemoteIce(req_id, variant, Err(RpcError::new2(WebrtcError::RpcEndpointNotFound)))));
                        }
                        ExtIn::RestartIce(req_id, app, variant, remote, useragent, req, extra_data, record) => {
                            let sdp = req.sdp.clone();
                            let session_id = gen_cluster_session_id(); //TODO need to reuse old session_id
                            if let Ok((ice_lite, sdp, index)) = self.spawn(app, remote, session_id, VariantParams::Webrtc(useragent, req, extra_data, record, self.secure.clone()), &sdp) {
                                self.queue.push_back(GroupOutput::Ext(index.into(), ExtOut::RestartIce(req_id, variant, Ok((ice_lite, sdp)))));
                            } else {
                                self.queue
                                    .push_back(GroupOutput::Ext(owner, ExtOut::RestartIce(req_id, variant, Err(RpcError::new2(WebrtcError::RpcEndpointNotFound)))));
                            }
                        }
                        ExtIn::Disconnect(req_id, variant) => {
                            self.queue
                                .push_back(GroupOutput::Ext(owner, ExtOut::Disconnect(req_id, variant, Err(RpcError::new2(WebrtcError::RpcEndpointNotFound)))));
                        }
                    }
                }
            }
        }
    }

    pub fn shutdown(&mut self, now: Instant) {
        if !self.shutdown {
            log::info!("[MediaWorkerWebrtc] shutdown request");
            self.shutdown = true;
            self.endpoints.on_shutdown(now);
        }
    }
}

impl<ES: MediaEdgeSecure> TaskSwitcherChild<GroupOutput> for MediaWorkerWebrtc<ES> {
    type Time = Instant;

    fn empty_event(&self) -> GroupOutput {
        GroupOutput::OnResourceEmpty
    }

    fn is_empty(&self) -> bool {
        self.shutdown && self.queue.is_empty() && self.endpoints.is_empty()
    }

    fn pop_output(&mut self, now: Instant) -> Option<GroupOutput> {
        return_if_some!(self.queue.pop_front());
        let (index, out) = match self.endpoints.pop_output(now)? {
            TaskGroupOutput::TaskOutput(index, out) => (index, out),
            TaskGroupOutput::OnResourceEmpty => return Some(GroupOutput::Continue),
        };
        Some(self.process_output(index, out))
    }
}
