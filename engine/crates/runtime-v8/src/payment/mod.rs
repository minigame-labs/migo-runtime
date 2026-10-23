//! Payment APIs: checkIsSupportMidasPayment, requestMidasPayment,
//! requestMidasPaymentGameItem.
//!
//! checkIsSupportMidasPayment is Mode B (sync, returns JSON).
//! requestMidasPayment/requestMidasPaymentGameItem are Mode C (async callback).

use deno_core::{Extension, OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;

#[op2]
#[string]
pub fn op_check_is_support_midas_payment(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<String, JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.payment() {
            return svc
                .check_is_support_midas_payment(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    // Default: payment not supported
    Ok(r#"{"data":{"allow_pay":false}}"#.to_string())
}

#[op2(fast)]
pub fn op_request_midas_payment(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.payment() {
            return svc
                .request_midas_payment(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "requestMidasPayment:fail not supported",
    ))
}

#[op2(fast)]
pub fn op_request_midas_payment_game_item(
    state: &mut OpState,
    #[string] options_json: String,
) -> Result<(), JsErrorBox> {
    let host = state.borrow::<HostOpState>();
    if let Some(ref services) = host.device_services {
        if let Some(svc) = services.payment() {
            return svc
                .request_midas_payment_game_item(&options_json)
                .map_err(JsErrorBox::generic);
        }
    }
    Err(JsErrorBox::generic(
        "requestMidasPaymentGameItem:fail not supported",
    ))
}

main_runtime_extension!(
    host_v8_payment,
    {
        deps = [host_v8_base],
        ops = [
            op_check_is_support_midas_payment,
            op_request_midas_payment,
            op_request_midas_payment_game_item,
        ],
    },
    esm_entry_point = "ext:host_v8_payment/99_global_scope.js",
    esm = [
        dir "src/payment",
        "01_payment.js",
        "99_global_scope.js",
    ],
);

pub fn payment_extensions() -> Vec<Extension> {
    vec![host_v8_payment::init()]
}
