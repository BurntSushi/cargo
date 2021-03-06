extern crate curl;
extern crate serialize;

use std::fmt;
use std::io::{mod, fs, MemReader, MemWriter, File};
use std::collections::HashMap;
use std::io::util::ChainedReader;
use std::result;

use curl::http;
use serialize::json;

pub struct Registry {
    host: String,
    token: String,
    handle: http::Handle,
}

pub type Result<T> = result::Result<T, Error>;

pub enum Error {
    CurlError(curl::ErrCode),
    NotOkResponse(http::Response),
    NonUtf8Body,
    ApiErrors(Vec<String>),
    Unauthorized,
    IoError(io::IoError),
}

#[deriving(Encodable)]
pub struct NewCrate {
    pub name: String,
    pub vers: String,
    pub deps: Vec<NewCrateDependency>,
    pub features: HashMap<String, Vec<String>>,
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub documentation: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
    pub keywords: Vec<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
}

#[deriving(Encodable)]
pub struct NewCrateDependency {
    pub optional: bool,
    pub default_features: bool,
    pub name: String,
    pub features: Vec<String>,
    pub version_req: String,
    pub target: Option<String>,
}

#[deriving(Decodable)] struct R { ok: bool }
#[deriving(Decodable)] struct ApiErrorList { errors: Vec<ApiError> }
#[deriving(Decodable)] struct ApiError { detail: String }
#[deriving(Encodable)] struct OwnersReq<'a> { users: &'a [&'a str] }

impl Registry {
    pub fn new(host: String, token: String) -> Registry {
        Registry::new_handle(host, token, http::Handle::new())
    }

    pub fn new_handle(host: String, token: String,
                      handle: http::Handle) -> Registry {
        Registry {
            host: host,
            token: token,
            handle: handle,
        }
    }

    pub fn add_owners(&mut self, krate: &str, owners: &[&str]) -> Result<()> {
        let body = json::encode(&OwnersReq { users: owners });
        let body = try!(self.put(format!("/crates/{}/owners", krate),
                                 body.as_bytes()));
        assert!(json::decode::<R>(body.as_slice()).unwrap().ok);
        Ok(())
    }

    pub fn remove_owners(&mut self, krate: &str, owners: &[&str]) -> Result<()> {
        let body = json::encode(&OwnersReq { users: owners });
        let body = try!(self.delete(format!("/crates/{}/owners", krate),
                                    Some(body.as_bytes())));
        assert!(json::decode::<R>(body.as_slice()).unwrap().ok);
        Ok(())
    }

    pub fn publish(&mut self, krate: &NewCrate, tarball: &Path) -> Result<()> {
        let json = json::encode(krate);
        // Prepare the body. The format of the upload request is:
        //
        //      <le u32 of json>
        //      <json request> (metadata for the package)
        //      <le u32 of tarball>
        //      <source tarball>
        let stat = try!(fs::stat(tarball).map_err(IoError));
        let header = {
            let mut w = MemWriter::new();
            w.write_le_u32(json.len() as u32).unwrap();
            w.write_str(json.as_slice()).unwrap();
            w.write_le_u32(stat.size as u32).unwrap();
            MemReader::new(w.unwrap())
        };
        let tarball = try!(File::open(tarball).map_err(IoError));
        let size = stat.size as uint + header.get_ref().len();
        let mut body = ChainedReader::new(vec![box header as Box<Reader>,
                                               box tarball as Box<Reader>].into_iter());

        let url = format!("{}/api/v1/crates/new", self.host);
        let response = handle(self.handle.put(url, &mut body)
                                         .content_length(size)
                                         .header("Authorization",
                                                 self.token.as_slice())
                                         .header("Accept", "application/json")
                                         .exec());
        let _body = try!(response);
        Ok(())
    }

    pub fn yank(&mut self, krate: &str, version: &str) -> Result<()> {
        let body = try!(self.delete(format!("/crates/{}/{}/yank", krate, version),
                                    None));
        assert!(json::decode::<R>(body.as_slice()).unwrap().ok);
        Ok(())
    }

    pub fn unyank(&mut self, krate: &str, version: &str) -> Result<()> {
        let body = try!(self.put(format!("/crates/{}/{}/unyank", krate, version),
                                 []));
        assert!(json::decode::<R>(body.as_slice()).unwrap().ok);
        Ok(())
    }

    fn put(&mut self, path: String, b: &[u8]) -> Result<String> {
        handle(self.handle.put(format!("{}/api/v1{}", self.host, path), b)
                          .header("Authorization", self.token.as_slice())
                          .header("Accept", "application/json")
                          .content_type("application/json")
                          .exec())
    }

    fn delete(&mut self, path: String, b: Option<&[u8]>) -> Result<String> {
        let mut req = self.handle.delete(format!("{}/api/v1{}", self.host, path))
                                 .header("Authorization", self.token.as_slice())
                                 .header("Accept", "application/json")
                                 .content_type("application/json");
        match b {
            Some(b) => req = req.body(b),
            None => {}
        }
        handle(req.exec())
    }
}

fn handle(response: result::Result<http::Response, curl::ErrCode>)
          -> Result<String> {
    let response = try!(response.map_err(CurlError));
    match response.get_code() {
        0 => {} // file upload url sometimes
        200 => {}
        403 => return Err(Unauthorized),
        _ => return Err(NotOkResponse(response))
    }

    let body = match String::from_utf8(response.move_body()) {
        Ok(body) => body,
        Err(..) => return Err(NonUtf8Body),
    };
    match json::decode::<ApiErrorList>(body.as_slice()) {
        Ok(errors) => {
            return Err(ApiErrors(errors.errors.into_iter().map(|s| s.detail)
                                       .collect()))
        }
        Err(..) => {}
    }
    Ok(body)
}

impl fmt::Show for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            NonUtf8Body => write!(f, "reponse body was not utf-8"),
            CurlError(ref err) => write!(f, "http error: {}", err),
            NotOkResponse(ref resp) => {
                write!(f, "failed to get a 200 OK response: {}", resp)
            }
            ApiErrors(ref errs) => {
                write!(f, "api errors: {}", errs.connect(", "))
            }
            Unauthorized => write!(f, "unauthorized API access"),
            IoError(ref e) => write!(f, "io error: {}", e),
        }
    }
}
