use anyhow::Result;
use axum::{Router, body::Body, extract::ConnectInfo, http::Request, response::IntoResponse};
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
pub async fn serve(listener: TcpListener, router: Router, stop: CancellationToken) -> Result<()> {
    let slots = Arc::new(Semaphore::new(256));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
         _=stop.cancelled()=>break,
         Some(_)=tasks.join_next()=>{},
         accepted=listener.accept()=>{
          let (socket,peer)=accepted?;
          let Ok(slot)=slots.clone().try_acquire_owned()else{drop(socket);continue};
          let router=router.clone();let stop=stop.clone();
          tasks.spawn(async move{
           let _slot=slot;let _=socket.set_nodelay(true);
           let service=service_fn(move|req:Request<hyper::body::Incoming>|{
            let router=router.clone();
            async move{
             let mut req=req.map(Body::new);req.extensions_mut().insert(ConnectInfo(peer));
             let length=req.headers().iter().map(|(k,v)|k.as_str().len()+v.len()+4).sum::<usize>();
             if length>16384||req.uri().to_string().len()>8192{return Ok::<_,Infallible>(axum::http::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE.into_response())}
             match tokio::time::timeout(Duration::from_secs(25),router.oneshot(req)).await{
              Ok(r)=>r,
              Err(_)=>Ok(axum::http::StatusCode::REQUEST_TIMEOUT.into_response())
             }
            }
           });
           let mut builder=hyper::server::conn::http1::Builder::new();
           builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5)).max_headers(64).max_buf_size(32768).keep_alive(false);
           let conn=builder.serve_connection(TokioIo::new(socket),service).with_upgrades();
           tokio::select!{_=stop.cancelled()=>{},_=conn=>{}}
          });
         }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
