//! Actix web mtbl example

use actix::actors::signal;
use actix::prelude::*;
use actix_web::{
    http, middleware::cors::Cors, App, AsyncResponder, Error, FutureResponse, HttpRequest,
    HttpResponse, Json, fs,
};
use futures;
use futures::future::Future;
use juniper;
use sled;
use slog;
use slog::Drain;
use slog::{info, o};
use slog_async;
use slog_term;

mod logger;
mod mt;

use crate::logger::ThreadLocalDrain;
use crate::mt::{SledExecutor};

// make git sha available in the program
include!(concat!(env!("OUT_DIR"), "/version.rs"));

/// State with SledExecutor address
struct State {
    mt: actix::Addr<SledExecutor>,
    logger: slog::Logger,
}

fn start_http(mt_addr: actix::Addr<SledExecutor>, logger: slog::Logger) {
    actix_web::server::HttpServer::new(move || {
        App::with_state(State {
            mt: mt_addr.clone(),
            logger: logger.clone(),
        })
        .configure(|app| {
            Cors::for_app(app)
                .send_wildcard()
                .allowed_methods(vec!["GET", "POST"])
                .allowed_header(http::header::CONTENT_TYPE)
                .max_age(3600)
                .resource("/graphql", |r| {
                    r.method(http::Method::POST).with(graphql);
                })
                .resource("/graphiql", |r| r.method(http::Method::GET).h(graphiql))

                .register()
        }).handler("/", fs::StaticFiles::new("./frontend/dist/").unwrap().index_file("index.html"))
    })
    .bind("0.0.0.0:63333")
    .unwrap()
    .start();
}

fn graphiql(_req: &HttpRequest<State>) -> Result<HttpResponse, Error> {
    let html = juniper::graphiql::graphiql_source("http://localhost:63333/graphql");
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html))
}

fn graphql(
    (st, data): (actix_web::State<State>, Json<mt::GraphQLData>),
) -> FutureResponse<HttpResponse> {
    st.mt
        .send(data.0)
        .from_err()
        .and_then(|res| match res {
            Ok(user) => Ok(HttpResponse::Ok()
                .content_type("application/json")
                .body(user)),
            Err(_) => Ok(HttpResponse::InternalServerError().into()),
        })
        .responder()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    //--- set up slog

    // set up terminal logging
    let decorator = slog_term::TermDecorator::new().build();
    let term_drain = slog_term::CompactFormat::new(decorator).build().fuse();

    // json log file
    let logfile = std::fs::File::create("/tmp/actix-test.log").unwrap();
    let json_drain = slog_json::Json::new(logfile)
        .add_default_keys()
        // include source code location
        .add_key_value(o!("place" =>
           slog::FnValue(move |info| {
               format!("{}::({}:{})",
                       info.module(),
                       info.file(),
                       info.line(),
                )}),
                "sha"=>VERGEN_SHA_SHORT))
        .build()
        .fuse();

    // duplicate log to both terminal and json file
    let dup_drain = slog::Duplicate::new(json_drain, term_drain);
    // make it async
    let async_drain = slog_async::Async::new(dup_drain.fuse()).build();
    // and add thread local logging
    let log = slog::Logger::root(ThreadLocalDrain { drain: async_drain }.fuse(), o!());
    let tree = sled::Db::start_default("countries_db")?.open_tree(b"countries".to_vec())?;
    //--- end of slog setup
    actix::System::run(move || {
        // set up MTBL lookup thread
        let mt_logger = log.new(o!("thread_name"=>"mtbl"));

        // Start mtbl executor actors
        let addr = SyncArbiter::start(3, move || {
            SledExecutor::new(tree.clone(), mt_logger.new(o!()))
        });

        // Start http server in its own thread
        let http_logger = log.new(o!("thread_name"=>"http"));
        start_http(addr, http_logger);
        info!(log, "Started http server");

        // handle signals
        let _ = signal::DefaultSignalsHandler::start_default();
    });
    Ok(())
}
