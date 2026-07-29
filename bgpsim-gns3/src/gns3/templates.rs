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

//! Module for managing GNS3 templates

use log::warn;
use rustify::{blocking::clients::reqwest::Client, Endpoint};
use rustify_derive::Endpoint;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Gns3Error, LoggingClient};

const FRR_TEMPLATE_NAME: &str = "FRR-docker";
const FRR_TEMPLATE_EXTRA_VOLUME: &str = "/etc/frr";
const FRR_TEMPLATE_IMAGE: &str = "frr:latest";
const FRR_TEMPLATE_START_COMMAND: &str = "/gns3/bin/busybox sh -c /startup.sh";

const IPTERM_TEMPLATE_NAME: &str = "IPTerm";
const IPTERM_TEMPLATE_IMAGE: &str = "gns3/ipterm:latest";

/// Get the UUID for the FRR template. If the template does not yet exist, create it.
pub fn get_or_create_frr_template(
    client: &Client,
    specific_template: Option<(&str, &str)>,
) -> Result<Uuid, Gns3Error> {
    let (name, image) = specific_template.unwrap_or((FRR_TEMPLATE_NAME, FRR_TEMPLATE_IMAGE));
    let templates = client.exec(GetTemplates {})?;

    for template in templates {
        if template.name == name {
            if template.start_command == Some(FRR_TEMPLATE_START_COMMAND.to_string())
                && template.extra_volumes == Some(vec![FRR_TEMPLATE_EXTRA_VOLUME.to_string()])
                && template.image == Some(image.to_string())
            {
                return Ok(template.template_id);
            } else {
                return Err(Gns3Error::InvalidFrrTemplate(
                    name.to_string(),
                    template.image.unwrap(),
                    image.to_string(),
                ));
            }
        }
    }

    warn!("Could not find a template with name: {}, creating one with image {}", name, image);

    Ok(client
        .exec(NewDockerTemplate {
            compute_id: "local".to_string(),
            name: name.to_string(),
            template_type: TemplateType::Docker,
            category: Some("guest".to_string()),
            adapters: 8,
            symbol: Some(":/symbols/affinity/circle/blue/router2.svg".to_string()),
            environment: None,
            console_type: "telnet".to_string(),
            image: image.to_string(),
            extra_volumes: vec![FRR_TEMPLATE_EXTRA_VOLUME.to_string()],
            start_command: Some(FRR_TEMPLATE_START_COMMAND.to_string()),
        })?
        .template_id)
}

/// Get the UUID for the IPTerm template. If the template does not yet exist, create it.
pub fn get_or_create_ipterm_template(client: &Client) -> Result<Uuid, Gns3Error> {
    let templates = GetTemplates {}.exec_block(client)?.parse()?;

    for template in templates {
        if template.name == IPTERM_TEMPLATE_NAME {
            if template.start_command.unwrap_or_default() == *""
                && template.extra_volumes.unwrap_or_default().is_empty()
                && template.adapters == Some(1)
                && template.image == Some(IPTERM_TEMPLATE_IMAGE.to_string())
            {
                return Ok(template.template_id);
            } else {
                return Err(Gns3Error::InvalidIptermTemplate);
            }
        }
    }

    Ok(NewDockerTemplate {
        compute_id: "local".to_string(),
        name: IPTERM_TEMPLATE_NAME.to_string(),
        template_type: TemplateType::Docker,
        category: Some("guest".to_string()),
        adapters: 1,
        symbol: Some(":/symbols/affinity/circle/gray/client.svg".to_string()),
        environment: None,
        console_type: "telnet".to_string(),
        image: IPTERM_TEMPLATE_IMAGE.to_string(),
        extra_volumes: vec![],
        start_command: None,
    }
    .exec_block(client)?
    .parse()?
    .template_id)
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "templates", method = "GET", response = "Vec<Gns3Template>")]
pub struct GetTemplates {}

#[derive(Debug, Deserialize)]
pub struct Gns3Template {
    pub name: String,
    pub template_id: Uuid,
    pub compute_id: Option<String>,
    pub template_type: TemplateType,
    pub extra_volumes: Option<Vec<String>>,
    pub image: Option<String>,
    pub adapters: Option<usize>,
    pub builtin: bool,
    pub start_command: Option<String>,
}

#[derive(Debug, Endpoint)]
#[endpoint(path = "templates", method = "POST", response = "Gns3Template")]
pub struct NewDockerTemplate {
    pub compute_id: String,
    pub name: String,
    pub template_type: TemplateType,
    pub category: Option<String>,
    pub adapters: usize,
    pub symbol: Option<String>,
    pub environment: Option<String>,
    pub console_type: String,
    pub image: String,
    pub extra_volumes: Vec<String>,
    pub start_command: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateType {
    Cloud,
    EthernetHub,
    EthernetSwitch,
    Docker,
    Dynamips,
    Vpcs,
    Traceng,
    Virtualbox,
    Vmware,
    Iou,
    Qemu,
    Nat,
    FrameRelaySwitch,
    AtmSwitch,
}
