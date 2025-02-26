use poem::{http::HeaderValue, Endpoint, Middleware, Request, Result};

pub struct ContentTypeCleanerMiddleware;

impl<E: Endpoint> Middleware<E> for ContentTypeCleanerMiddleware {
    type Output = ContentTypeCleaner<E>;

    fn transform(&self, ep: E) -> Self::Output {
        ContentTypeCleaner { ep }
    }
}

pub struct ContentTypeCleaner<E> {
    ep: E,
}

impl<E: Endpoint> Endpoint for ContentTypeCleaner<E> {
    type Output = E::Output;

    async fn call(&self, mut req: Request) -> Result<Self::Output> {
        log::info!("Request headers:");

        log::info!("[Request] Path: {} {}", req.method(), req.uri().path());

        for (name, value) in req.headers().iter() {
            log::info!("[Request] {}: {}", name, value.to_str().unwrap_or("<invalid value>"));
        }

        // Enhanced body logging
        let body_bytes = req.take_body().into_bytes().await?;
        log::info!("[Request] Body size: {} bytes", body_bytes.len());

        if !body_bytes.is_empty() {
            match String::from_utf8(body_bytes.to_vec()) {
                Ok(body_str) => {
                    log::info!("[Request] Body (text): {}", body_str);
                }
                Err(_) => {
                    // Log binary data in hex format
                    log::info!("[Request] Body (binary): {:02x?}", &body_bytes[..std::cmp::min(body_bytes.len(), 1024)]);
                }
            }
        }

        // Restore body
        req.set_body(body_bytes);

        if let Some(content_type) = req.headers().get("content-type") {
            log::info!("Content-Type: {}", content_type.to_str().unwrap_or("<invalid value>"));
            let clean_type = content_type.to_str().unwrap_or("").split(';').next().unwrap_or("").trim();

            if clean_type == "application/sdp" {
                req.headers_mut().insert("content-type", HeaderValue::from_static("application/sdp"));
            }
        }
        self.ep.call(req).await
    }
}
