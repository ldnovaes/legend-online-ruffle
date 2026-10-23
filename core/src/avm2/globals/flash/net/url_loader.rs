//! `flash.net.URLLoader` native function definitions

use crate::avm2::activation::Activation;
use crate::avm2::function::FunctionArgs;
use crate::avm2::globals::flash::display::loader::request_from_url_request;
use crate::avm2::parameters::ParametersExt;
use crate::avm2::value::Value;
use crate::avm2::object::TObject as _;
use crate::avm2::{Error, Object};

/// Native function definition for `URLLoader.load`
pub fn load<'gc>(
    activation: &mut Activation<'_, 'gc>,
    this: Value<'gc>,
    args: FunctionArgs<'_, 'gc>,
) -> Result<Value<'gc>, Error<'gc>> {
    let this = this.as_object().unwrap();

    let request = args.get_object(activation, 0, "request")?;

    spawn_fetch(activation, this, request)
}

fn spawn_fetch<'gc>(
    activation: &mut Activation<'_, 'gc>,
    loader_object: Object<'gc>,
    url_request: Object<'gc>,
) -> Result<Value<'gc>, Error<'gc>> {
    use crate::avm2::globals::slots::flash_net_url_loader as url_loader_slots;

    let request = request_from_url_request(activation, url_request)?;

    // Starting a new load cancels any load still in progress on this URLLoader.
    let load_id = loader_object
        .get_slot(url_loader_slots::_LOAD_ID)
        .as_u32()
        .wrapping_add(1);
    loader_object.set_slot(url_loader_slots::_LOAD_ID, load_id.into(), activation)?;

    let future = crate::loader::load_data_into_url_loader(
        activation.context,
        loader_object.as_script_object().unwrap(),
        request,
        load_id,
    );
    activation.context.navigator.spawn_future(future);
    Ok(Value::Undefined)
}
