// BgpSim-GNS3: Control and interact with GNS3 from BgpSim
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

//! To run the tests, make sure that a gns3 instance is running on the localhost with port 3080.

use itertools::Itertools;
use rustify::errors::ClientError;

use crate::gns3::Gns3Error;

mod create_links;
mod create_nodes;
mod create_project;
mod frr;
mod ipterm;
mod network;
mod updates;

#[macro_export]
macro_rules! e {
    ($x: expr) => {
        $crate::test::expect($x).unwrap()
    };
}

pub(self) fn expect<T>(inp: Result<T, Gns3Error>) -> Option<T> {
    match inp {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("Error: {e}");
            if let Gns3Error::ApiError(e) = e {
                match e {
                    ClientError::DataParseError { source }
                    | ClientError::EndpointBuildError { source }
                    | ClientError::GenericError { source }
                    | ClientError::UrlQueryParseError { source }
                    | ClientError::ResponseError { source } => {
                        eprintln!("{}", source.root_cause());
                    }
                    ClientError::RequestError { source, url, method } => {
                        eprintln!("Request: {}, {}: \n{}", method, url, source.root_cause())
                    }
                    ClientError::RequestBuildError { source, method, url } => {
                        eprintln!("Request: {method:?}, {url}:\n{source}")
                    }
                    ClientError::ResponseConversionError { source, content } => eprintln!(
                        "{}\nContent:\n{}",
                        source.root_cause(),
                        String::from_utf8_lossy(&content)
                    ),
                    ClientError::ResponseParseError { source, content } => {
                        let lined_response = content
                            .unwrap_or_default()
                            .lines()
                            .enumerate()
                            .map(|(i, l)| format!(" {i:>3} | {l}"))
                            .join("\n");
                        eprintln!("{}\nResponse:\n{}", source.root_cause(), lined_response);
                    }
                    ClientError::ServerResponseError { code, content } => {
                        eprintln!("Code: {}\nResponse:\n{}", code, content.unwrap_or_default())
                    }
                    _ => {}
                }
            }
            None
        }
    }
}
