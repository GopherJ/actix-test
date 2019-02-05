//! Actix web mtbl example

extern crate actix;
extern crate actix_web;

extern crate futures;
extern crate serde;
extern crate serde_cbor;
extern crate serde_json;
extern crate serde_yaml;

use actix::actors::signal;
use actix::*;
use actix_web::http;
use actix_web::*;

use futures::future::Future;

#[macro_use]
extern crate slog;
extern crate slog_async;
extern crate slog_json;
extern crate slog_term;

extern crate mtbl;

#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate tera;

use http::header;
use slog::Drain;
use std::sync::Arc;

mod logger;
mod mt;

use logger::ThreadLocalDrain;
use mt::{GetCountry, MtblExecutor};

// make git sha available in the program
include!(concat!(env!("OUT_DIR"), "/version.rs"));

/// State with MtblExecutor address
struct State {
    mt: actix::Addr<MtblExecutor>,
    logger: slog::Logger,
}

fn start_http(mt_addr: actix::Addr<MtblExecutor>, logger: slog::Logger) {
    actix_web::server::HttpServer::new(move || {
        App::with_state(State {
            mt: mt_addr.clone(),
            logger: logger.clone(),
        })
        .resource("/{name}", |r| r.method(http::Method::GET).with_async(index))
    })
    .bind("0.0.0.0:63333")
    .unwrap()
    .start();
}

lazy_static! {
    pub static ref TEMPLATES: tera::Tera = {
        let mut t = compile_templates!("templates/**/*");
        t.autoescape_on(vec!["html"]);
        t
    };
}

// Async request handler
fn index(req: HttpRequest<State>) -> Box<Future<Item = HttpResponse, Error = Error>> {
    let name = &req.match_info()["name"];
    let guard = logger::FnGuard::new(
        req.state().logger.clone(),
        o!("name"=>name.to_owned()),
        "index",
    );
    let accept_hdr = get_accept_str(req.headers().get(header::ACCEPT));

    req.state()
        .mt
        .send(GetCountry {
            name: name.to_owned(),
        })
        .from_err()
        .and_then(move |res| match res {
            Ok(country) => match country {
                Some(c) => make_response(guard, accept_hdr, c),
                None => Ok(HttpResponse::NotFound().finish()),
            },
            Err(_) => Ok(HttpResponse::InternalServerError().finish()),
        })
        .responder()
}

fn make_response(
    log: logger::FnGuard,
    accept: String,
    object: serde_cbor::Value,
) -> std::result::Result<actix_web::HttpResponse, actix_web::Error> {
    let _guard = log.sub_guard("make_response");
    let mut res = HttpResponse::Ok();
    match accept.as_str() {
        "application/yaml" => Ok(res
            .content_type("application/yaml")
            .body(serde_yaml::to_string(&object).unwrap())),
        "application/json" => Ok(res.json(&object)),
        _ => Ok(res
            .content_type("text/html")
            .body(TEMPLATES.render("country.html", &object).unwrap())),
    }
}

fn get_accept_str(hdr: Option<&http::header::HeaderValue>) -> String {
    let html = "text/html".to_string();
    match hdr {
        Some(h) => match h.to_str() {
            Ok(st) => st.to_string(),
            _ => html,
        },
        None => html,
    }
}

fn main() {
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

    //--- end of slog setup
    actix::System::run(move || {
        // set up MTBL lookup thread
        let mt_logger = log.new(o!("thread_name"=>"mtbl"));
        let reader = Arc::new(mtbl::Reader::open_from_path("countries.mtbl").unwrap());
        // Start mtbl executor actors
        let addr = SyncArbiter::start(3, move || MtblExecutor {
            reader: reader.clone(),
            logger: mt_logger.new(o!()),
        });

        // Start http server in its own thread
        let http_logger = log.new(o!("thread_name"=>"http"));
        start_http(addr, http_logger);
        info!(log, "Started http server");

        // handle signals
        let _ = signal::DefaultSignalsHandler::start_default();
    });
}
