#[macro_use]
extern crate serde_derive;

use actix_cors::Cors;
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use actix_web_prom::PrometheusMetrics;

use env_logger;
use geoipupdate::MaxMindReader;
use maxminddb::geoip2::City;
use prometheus::{opts, IntCounter, IntGauge};

use std::net::SocketAddr;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::RwLock;

use structopt::StructOpt;

use error::GeoIPError;

use geoipupdate::GeoIPUpdater;
use geoipupdate::GeoIPUpdaterMetrics;

mod error;
mod geoipupdate;

#[derive(StructOpt, Debug, Clone)]
struct Options {
    #[structopt(
        short,
        long,
        env = "GEOIP_API_LISTEN_ADDR",
        default_value = "127.0.0.1:3000"
    )]
    listen_addr: SocketAddr,
    #[structopt(long, env = "GEOIP_API_UPDATE_MINUTES", default_value = "30")]
    update_minutes: u64,
    #[structopt(long, env = "GEOIP_API_ACCOUNT_ID")]
    account_id: String,
    #[structopt(long, env = "GEOIP_API_LICENSE_KEY")]
    license_key: String,
    #[structopt(long, env = "GEOIP_API_EDITION_ID", default_value = "GeoIP2-City")]
    edition_id: String,
}

#[derive(Serialize)]
struct ResolvedIPResponse<'a> {
    pub ip_address: &'a str,
    pub latitude: &'a f64,
    pub longitude: &'a f64,
    pub postal_code: &'a str,
    pub continent_code: &'a str,
    pub continent_name: &'a str,
    pub country_code: &'a str,
    pub country_name: &'a str,
    pub region_code: &'a str,
    pub region_name: &'a str,
    pub province_code: &'a str,
    pub province_name: &'a str,
    pub city_name: &'a str,
    pub timezone: &'a str,
}

#[derive(Deserialize, Debug)]
struct QueryParams {
    ip: Option<String>,
    lang: Option<String>,
}

fn get_language(lang: Option<String>) -> String {
    lang.unwrap_or_else(|| String::from("en"))
}

async fn index<'a>(
    _req: HttpRequest,
    db: web::Data<RwLock<MaxMindReader>>,
    web::Query(query): web::Query<QueryParams>,
) -> Result<HttpResponse, GeoIPError> {
    let language = get_language(query.lang);
    let language = language.as_str();
    let ip_address = query
        .ip
        .filter(|ip_address| {
            ip_address.parse::<Ipv4Addr>().is_ok() || ip_address.parse::<Ipv6Addr>().is_ok()
        })
        .ok_or(GeoIPError::ParseError)?;

    let db_opt = db.read().map_err(|_| GeoIPError::PoisonError)?;

    let geoip: City = if let Some(db) = &*db_opt {
        // we can unwrap here because we parsed previously
        db.lookup(ip_address.parse().unwrap())
    } else {
        Err(GeoIPError::DatabaseNotFound)?
    }?;

    let region = geoip
        .subdivisions
        .as_ref()
        .filter(|subdivs| !subdivs.is_empty())
        .and_then(|subdivs| subdivs.get(0));

    let province = geoip
        .subdivisions
        .as_ref()
        .filter(|subdivs| subdivs.len() > 1)
        .and_then(|subdivs| subdivs.get(1));

    let resp = ResolvedIPResponse {
        ip_address: &ip_address,
        latitude: geoip
            .location
            .as_ref()
            .and_then(|loc| loc.latitude.as_ref())
            .unwrap_or(&0.0),
        longitude: geoip
            .location
            .as_ref()
            .and_then(|loc| loc.longitude.as_ref())
            .unwrap_or(&0.0),
        postal_code: geoip.postal.and_then(|postal| postal.code).unwrap_or(""),
        continent_code: geoip
            .continent
            .as_ref()
            .and_then(|cont| cont.code)
            .unwrap_or(""),
        continent_name: geoip
            .continent
            .as_ref()
            .and_then(|cont| cont.names.as_ref())
            .and_then(|names| names.get(language))
            .map(|x| *x)
            .unwrap_or(""),
        country_code: geoip
            .country
            .as_ref()
            .and_then(|country| country.iso_code)
            .unwrap_or(""),
        country_name: geoip
            .country
            .as_ref()
            .and_then(|country| country.names.as_ref())
            .and_then(|names| names.get(language))
            .map(|x| *x)
            .unwrap_or(""),
        region_code: region.and_then(|subdiv| subdiv.iso_code).unwrap_or(""),
        region_name: region
            .as_ref()
            .and_then(|subdiv| subdiv.names.as_ref())
            .and_then(|names| names.get(language))
            .map(|x| *x)
            .unwrap_or(""),
        province_code: province
            .as_ref()
            .and_then(|subdiv| subdiv.iso_code)
            .unwrap_or(""),
        province_name: province
            .as_ref()
            .and_then(|subdiv| subdiv.names.as_ref())
            .and_then(|names| names.get(language))
            .map(|x| *x)
            .unwrap_or(""),
        city_name: geoip
            .city
            .as_ref()
            .and_then(|city| city.names.as_ref())
            .and_then(|names| names.get(&language))
            .map(|x| *x)
            .unwrap_or(""),
        timezone: geoip
            .location
            .as_ref()
            .and_then(|loc| loc.time_zone)
            .unwrap_or(""),
    };

    Ok(HttpResponse::Ok().json(resp))
}

async fn health(
    _req: HttpRequest,
    db: web::Data<RwLock<MaxMindReader>>,
) -> Result<&'static str, GeoIPError> {
    let db_opt = db.read().map_err(|_| GeoIPError::PoisonError)?;

    if let Some(_db) = &*db_opt {
        Ok("OK")
    } else {
        Err(GeoIPError::DatabaseNotFound)?
    }
}

#[actix_rt::main]
async fn main() {
    env_logger::init();
    let opt = Options::from_args();
    let listen_addr = opt.listen_addr;

    let prometheus = PrometheusMetrics::new("geoip", Some("/metrics"), None);

    let last_updated_opts = opts!(
        "database_last_updated",
        "timestamp of the last time the geoip database was modified"
    )
    .namespace("geoip");
    let last_updated = IntGauge::with_opts(last_updated_opts).unwrap();

    let last_checked_opts = opts!(
        "database_last_checked",
        "timestamp of the last time an update was checked for the geoip database"
    )
    .namespace("geoip");
    let last_checked = IntGauge::with_opts(last_checked_opts).unwrap();

    let error_count_opts = opts!(
        "geoip_update_errors",
        "number of errors fetching the geoip database"
    )
    .namespace("geoip");
    let error_count = IntCounter::with_opts(error_count_opts).unwrap();

    prometheus
        .registry
        .register(Box::new(last_updated.clone()))
        .unwrap();

    prometheus
        .registry
        .register(Box::new(last_checked.clone()))
        .unwrap();

    prometheus
        .registry
        .register(Box::new(error_count.clone()))
        .unwrap();

    let db: web::Data<RwLock<MaxMindReader>> = web::Data::new(RwLock::new(None));

    let updater_metrics = GeoIPUpdaterMetrics::new(
        last_updated.clone(),
        last_checked.clone(),
        error_count.clone(),
    );

    let updater = GeoIPUpdater::new(
        opt.update_minutes,
        db.clone().into_inner(),
        opt.account_id,
        opt.license_key,
        opt.edition_id,
        updater_metrics,
    );
    updater.start();

    println!("Listening on http://{}", listen_addr);

    HttpServer::new(move || {
        App::new()
            .app_data(db.clone())
            .wrap(Cors::new().send_wildcard().finish())
            .wrap(prometheus.clone())
            .route("/", web::route().to(index))
            .route("/health", web::route().to(health))
    })
    .bind(opt.listen_addr)
    .unwrap_or_else(|_| panic!("Can not bind to {}", listen_addr))
    .run()
    .await
    .unwrap()
}
