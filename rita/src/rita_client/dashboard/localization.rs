use crate::SETTING;
use actix_web::error::JsonPayloadError;
use actix_web::{client, HttpMessage, HttpRequest, HttpResponse, Json};
use althea_types::WyreReservationRequestCarrier;
use althea_types::WyreReservationResponse;
use failure::Error;
use futures01::future;
use futures01::future::Either;
use futures01::Future;
use phonenumber::Mode;
use settings::client::RitaClientSettings;
use settings::localization::LocalizationSettings;
use settings::RitaCommonSettings;
use std::time::Duration;

/// A version of the localization struct that serializes into a more easily
/// consumable form
#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct LocalizationReturn {
    pub wyre_enabled: bool,
    pub wyre_account_id: String,
    pub display_currency_symbol: bool,
    pub support_number: String,
    pub wyre_reservation_flow: bool,
}

impl From<LocalizationSettings> for LocalizationReturn {
    fn from(input: LocalizationSettings) -> Self {
        LocalizationReturn {
            wyre_enabled: input.wyre_enabled,
            wyre_account_id: input.wyre_account_id,
            display_currency_symbol: input.display_currency_symbol,
            support_number: input
                .support_number
                .format()
                .mode(Mode::National)
                .to_string(),
            wyre_reservation_flow: input.wyre_reservation_flow,
        }
    }
}

pub fn get_localization(_req: HttpRequest) -> Json<LocalizationReturn> {
    debug!("/localization GET hit");
    let localization = SETTING.get_localization().clone();
    Json(localization.into())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AmountRequest {
    amount: f32,
}

pub fn get_wyre_reservation(
    amount: Json<AmountRequest>,
) -> Box<dyn Future<Item = HttpResponse, Error = Error>> {
    trace!("Getting wyre reservation");
    let exit_client = SETTING.get_exit_client();
    let operator = SETTING.get_operator();
    let payment = SETTING.get_payment();
    let payload = WyreReservationRequestCarrier {
        amount: amount.amount,
        address: payment.eth_address.unwrap(),
        // todo fix this
        contact_info: exit_client.contact_info.clone().unwrap().into(),
        // todo fix this
        billing_details: operator.billing_details.clone().unwrap(),
    };

    #[cfg(not(feature = "operator_debug"))]
    let api_url = "https://operator.althea.net:8080/wyre_reservation";
    #[cfg(feature = "operator_debug")]
    let api_url = "http://192.168.10.2:8080/wyre_reservation";
    Box::new(
        client::post(&api_url)
            .timeout(Duration::from_secs(10))
            .json(&payload)
            .unwrap()
            .send()
            .then(move |response| match response {
                Ok(response) => Either::A(response.json().then(
                    move |value: Result<WyreReservationResponse, JsonPayloadError>| match value {
                        Ok(value) => Ok(HttpResponse::Ok().json(value)),
                        Err(e) => {
                            trace!("Failed to deserialize wyre response {:?}", e);
                            Ok(HttpResponse::InternalServerError().finish())
                        }
                    },
                )),
                Err(e) => {
                    trace!("Failed to send wyre request {:?}", e);
                    Either::B(future::ok(HttpResponse::InternalServerError().finish()))
                }
            }),
    )
}
