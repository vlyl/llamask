use std::io::{self, Read};

use llamask_core::model::EntityType;
use llamask_core::sidecar::{
    SIDECAR_PROTOCOL_VERSION, SidecarFinding, SidecarRequest, SidecarResponse,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let request: SidecarRequest = serde_json::from_str(&input)?;
    let mut findings = Vec::new();
    let target = "星海科技有限公司";
    for (byte_start, _) in request.text.match_indices(target) {
        let start = request.text[..byte_start].chars().count();
        let end = start + target.chars().count();
        findings.push(SidecarFinding {
            start,
            end,
            entity_type: EntityType::OrgName,
            matched_text: target.to_owned(),
            confidence: 0.98,
            reason_code: "MOCK_ORGANIZATION".to_owned(),
        });
    }
    let response = SidecarResponse {
        protocol_version: SIDECAR_PROTOCOL_VERSION,
        request_id: request.request_id,
        findings,
        warnings: Vec::new(),
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
