use super::*;
use std::ffi::c_void;
use system_configuration::core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    boolean::CFBoolean,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use system_configuration::dynamic_store::SCDynamicStoreBuilder;
use system_configuration::sys::schema_definitions::{
    kSCPropNetProxiesExceptionsList, kSCPropNetProxiesExcludeSimpleHostnames,
    kSCPropNetProxiesHTTPEnable, kSCPropNetProxiesHTTPPort, kSCPropNetProxiesHTTPProxy,
    kSCPropNetProxiesHTTPSEnable, kSCPropNetProxiesHTTPSPort, kSCPropNetProxiesHTTPSProxy,
};

pub(super) fn read_system_proxy() -> Option<ProxyConfig> {
    let store = SCDynamicStoreBuilder::new("codex-companion").build()?;
    let settings = store.get_proxies()?;
    let exceptions = string_array(&settings, unsafe { kSCPropNetProxiesExceptionsList });
    let exclude_simple_hostnames = exceptions.iter().any(|entry| entry.trim() == "<local>")
        || boolean_value(&settings, unsafe {
            kSCPropNetProxiesExcludeSimpleHostnames
        });
    Some(ProxyConfig {
        http: proxy_url(
            &settings,
            unsafe { kSCPropNetProxiesHTTPEnable },
            unsafe { kSCPropNetProxiesHTTPProxy },
            unsafe { kSCPropNetProxiesHTTPPort },
        ),
        https: proxy_url(
            &settings,
            unsafe { kSCPropNetProxiesHTTPSEnable },
            unsafe { kSCPropNetProxiesHTTPSProxy },
            unsafe { kSCPropNetProxiesHTTPSPort },
        ),
        exceptions,
        exclude_simple_hostnames,
        ..Default::default()
    })
}

fn proxy_url(
    settings: &system_configuration::core_foundation::dictionary::CFDictionary<CFString, CFType>,
    enabled_key: CFStringRef,
    host_key: CFStringRef,
    port_key: CFStringRef,
) -> Option<String> {
    let enabled = settings
        .find(enabled_key)
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|value| value.to_i32())
        == Some(1);
    if !enabled {
        return None;
    }
    let host = settings
        .find(host_key)
        .and_then(|value| value.downcast::<CFString>())?
        .to_string();
    if host.trim().is_empty() {
        return None;
    }
    let port = settings
        .find(port_key)
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|value| value.to_i32());
    Some(match port {
        Some(port) => format!("http://{host}:{port}"),
        None => format!("http://{host}"),
    })
}

fn string_array(
    settings: &system_configuration::core_foundation::dictionary::CFDictionary<CFString, CFType>,
    key: CFStringRef,
) -> Vec<String> {
    let Some(array) = settings
        .find(key)
        .and_then(|value| value.downcast::<CFArray<*const c_void>>())
    else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|value| {
            let value = unsafe { CFType::wrap_under_get_rule(*value) };
            value.downcast::<CFString>().map(|value| value.to_string())
        })
        .collect()
}

fn boolean_value(
    settings: &system_configuration::core_foundation::dictionary::CFDictionary<CFString, CFType>,
    key: CFStringRef,
) -> bool {
    settings.find(key).is_some_and(|value| {
        value
            .downcast::<CFBoolean>()
            .map(bool::from)
            .or_else(|| {
                value
                    .downcast::<CFNumber>()
                    .and_then(|value| value.to_i32())
                    .map(|value| value != 0)
            })
            .unwrap_or(false)
    })
}
