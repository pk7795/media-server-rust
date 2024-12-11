use async_std::channel::{bounded, Receiver, Sender};
use metrics_dashboard::HttpMetricMiddleware;
use poem::{
    listener::{Listener, RustlsCertificate, RustlsConfig, TcpListener},
    middleware::Cors,
    EndpointExt, Route, Server,
};

mod authorization;
mod embedded_endpoint;
mod payload_sdp;
mod remote_ip_addr;
mod rpc_req;
mod user_agent;

pub use authorization::TokenAuthorization;
pub use embedded_endpoint::EmbeddedFilesEndpoint;
pub use payload_sdp::{ApplicationSdp, ApplicationSdpPatch, CustomHttpResponse};
pub use remote_ip_addr::RemoteIpAddr;
pub use rpc_req::RpcReqResHttp;
pub use user_agent::UserAgent;

// const CERT: &str = include_str!("../../belo.chat/cert.pem");
// const KEY: &str = include_str!("../../belo.chat/privkey.pem");

pub struct HttpRpcServer<R: Send> {
    port: u16,
    tls: bool,
    tx: Sender<R>,
    rx: Receiver<R>,
}

impl<R: 'static + Send> HttpRpcServer<R> {
    pub fn new(port: u16, tls: bool) -> Self {
        let (tx, rx) = bounded(100);
        Self { port, tls, tx, rx }
    }

    pub async fn start<CTX: Send + Sync + Clone + 'static>(&mut self, api_service: Route, ctx: CTX) {
        let cors = Cors::new().expose_header("Location");
        // read cert from file 
        let cert = std::fs::read_to_string("belo.chat/cert.pem").expect("cert.pem not found");
        let key = std::fs::read_to_string("belo.chat/privkey.pem").expect("privkey.pem not found");
        let route = Route::new().nest("/", api_service).with(cors).data((self.tx.clone(), ctx));
        if self.tls {
            let socket = TcpListener::bind(format!("0.0.0.0:{}", self.port)).rustls(RustlsConfig::new().fallback(RustlsCertificate::new().key(key).cert(cert)));

            log::info!("Listening https server on 0.0.0.0:{}", self.port);
            async_std::task::spawn(async move {
                Server::new(socket).run(route.with(HttpMetricMiddleware)).await.expect("Should run");
            });
        } else {
            let socket = TcpListener::bind(format!("0.0.0.0:{}", self.port));

            log::info!("Listening http server on 0.0.0.0:{}", self.port);
            async_std::task::spawn(async move {
                Server::new(socket).run(route.with(HttpMetricMiddleware)).await.expect("Should run");
            });
        }
    }

    pub async fn recv(&mut self) -> Option<R> {
        self.rx.recv().await.ok()
    }
}
