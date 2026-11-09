//! Addresses of operations, and links between them.
//!
//! `services.web.create_charge` names an operation in the same repository as
//! the file it is written in; `#billing:services.api.create_charge` names one
//! in the external service `#billing`. The form
//! `services.<service>.operations.<operation>` is accepted too. A documented
//! operation may be named by its method and path instead of its
//! `operationId`: `services.api.GET /v1/charges/{}`, path parameters written
//! `{}`.

/// Where an operation is declared.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    /// The external service (`#billing`) whose description declares it, or
    /// `None` for the file the address is written in.
    pub repository: Option<String>,
    /// The service.
    pub service: String,
    /// The operation.
    pub operation: String,
}

impl std::fmt::Display for Address {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(repository) = &self.repository {
            write!(formatter, "{repository}:")?;
        }
        write!(formatter, "services.{}.{}", self.service, self.operation)
    }
}

/// A link: the caller's operation, then the provider's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    /// The operation that calls.
    pub caller: Address,
    /// The operation called.
    pub provider: Address,
}

/// Why an address or link could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not {expected}")]
pub struct AddressError {
    /// What was written.
    pub text: String,
    /// What it should look like.
    pub expected: &'static str,
}

/// Reads `[#repository:]services.<service>.[operations.]<operation>`.
pub fn parse_address(text: &str) -> Result<Address, AddressError> {
    let error = || AddressError {
        text: text.to_owned(),
        expected: "an address like `services.<service>.<operation>` or `#repo:services.<service>.<operation>`",
    };
    let trimmed = text.trim().trim_matches(['"', '\'']);
    let (repository, path) = match trimmed.strip_prefix('#') {
        Some(rest) => {
            let (name, path) = rest.split_once(':').ok_or_else(error)?;
            (Some(format!("#{name}")), path)
        }
        None => (None, trimmed),
    };
    let parts: Vec<&str> = path.split('.').collect();
    let (service, operation) = match parts.as_slice() {
        ["services", service, operation] | ["services", service, "operations", operation] => {
            ((*service).to_owned(), (*operation).to_owned())
        }
        _ => split_route(path).ok_or_else(error)?,
    };
    if service.is_empty() || operation.is_empty() {
        return Err(error());
    }
    Ok(Address {
        repository,
        service,
        operation,
    })
}

/// The methods a route names.
const METHODS: [&str; 8] = [
    "GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE",
];

/// Whether an operation's name is a method and a path, as in
/// `GET /v1/charges/{}`.
pub fn is_route(operation: &str) -> bool {
    operation
        .split_once(' ')
        .is_some_and(|(method, path)| METHODS.contains(&method) && path.starts_with('/'))
}

/// The service and route of `services.<service>.[operations.]<METHOD /path>`,
/// whose path may hold dots.
fn split_route(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("services.")?;
    let (service, operation) = rest.split_once('.')?;
    let operation = operation.strip_prefix("operations.").unwrap_or(operation);
    (is_route(operation) && !service.is_empty()).then(|| (service.to_owned(), operation.to_owned()))
}

/// Reads `caller -> provider`.
pub fn parse_link(text: &str) -> Result<Link, AddressError> {
    let Some((caller, provider)) = text.split_once("->") else {
        return Err(AddressError {
            text: text.to_owned(),
            expected: "a link like `services.a.op -> \"#repo:services.b.op\"`",
        });
    };
    Ok(Link {
        caller: parse_address(caller)?,
        provider: parse_address(provider)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_external_addresses_are_read() {
        let link = parse_link(
            r##"services.web.create_charge -> "#billing:services.api.operations.create_charge""##,
        )
        .unwrap();

        assert_eq!(link.caller.repository, None);
        assert_eq!(link.caller.service, "web");
        assert_eq!(link.provider.repository.as_deref(), Some("#billing"));
        assert_eq!(link.provider.operation, "create_charge");
        assert_eq!(
            link.provider.to_string(),
            "#billing:services.api.create_charge"
        );
    }

    #[test]
    fn a_route_names_an_operation_by_method_and_path() {
        let address =
            parse_address(r##""#billing:services.api.GET /v1/files.upload/{}""##).unwrap();

        assert_eq!(address.service, "api");
        assert_eq!(address.operation, "GET /v1/files.upload/{}");
        assert!(parse_address("services.api.FETCH /x.y").is_err());
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        assert!(parse_address("web.create_charge").is_err());
        assert!(parse_address("#billing").is_err());
        assert!(parse_link("services.a.b").is_err());
    }
}
