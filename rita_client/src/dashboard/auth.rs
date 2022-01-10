use rita_common::KI;

use actix_web::{HttpResponse, Json};
use clarity::utils::bytes_to_hex_str;
use sha3::{Digest, Sha3_512};

use crate::RitaClientError;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct RouterPassword {
    pub password: String,
}

pub fn set_pass(router_pass: Json<RouterPassword>) -> Result<HttpResponse, RitaClientError> {
    debug!("/router/password hit with {:?}", router_pass);
    let router_pass = router_pass.into_inner();
    let input_string = router_pass.password.clone() + "RitaSalt";

    debug!("Using {} as sha3 512 input", input_string);
    let mut hasher = Sha3_512::new();
    hasher.update(input_string.as_bytes());
    let hashed_pass = bytes_to_hex_str(&hasher.finalize().to_vec());

    let mut rita_client = settings::get_rita_client();
    rita_client.network.rita_dashboard_password = Some(hashed_pass);

    if let Err(e) = settings::write_config() {
        return Err(RitaClientError::SettingsError(e));
    }

    if KI.is_openwrt() {
        KI.set_system_password(router_pass.password)?;

        // We edited disk contents, force global sync
        KI.fs_sync()?;
    }

    Ok(HttpResponse::Ok().json(()))
}

#[cfg(test)]
mod tests {
    use clarity::utils::bytes_to_hex_str;
    use hex_literal::hex;
    use sha3::{Digest, Sha3_512};

    #[test]
    fn test_hash() {
        let sha3_output = hex!("881c7d6ba98678bcd96e253086c4048c3ea15306d0d13ff48341c6285ee71102a47b6f16e20e4d65c0c3d677be689dfda6d326695609cbadfafa1800e9eb7fc1");

        let mut hasher = Sha3_512::new();
        hasher.update(b"testing");
        let result = hasher.finalize().to_vec();

        assert_eq!(result.len(), sha3_output.len());
        assert_eq!(result, sha3_output.to_vec());
    }

    #[test]
    fn test_hash_to_string() {
        let sha3sum_output = "881c7d6ba98678bcd96e253086c4048c3ea15306d0d13ff48341c6285ee71102a47b6f16e20e4d65c0c3d677be689dfda6d326695609cbadfafa1800e9eb7fc1";

        let mut hasher = Sha3_512::new();
        hasher.update(b"testing");
        let result = hasher.finalize().to_vec();

        assert_eq!(bytes_to_hex_str(&result), sha3sum_output);
    }
}
