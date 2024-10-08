use chrono::{offset::Utc, DateTime};
use linkerd_policy_controller_k8s_api::policy::Cidr;
use linkerd_policy_controller_k8s_api::{policy as linkerd_k8s_api, ResourceExt};
use std::net::IpAddr;

#[derive(Debug, Default)]
pub(crate) struct UnmeshedNetwork {
    pub networks: Vec<Cidr>,
    pub name: String,
    pub namespace: String,
    pub creation_timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
struct MatchedUnmeshedNetwork {
    matched_cidr: MatchedCidr,
    name: String,
    namespace: String,
    creation_timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
struct MatchedCidr(Cidr);

// === impl UnmeshedNetwork ===

impl From<linkerd_k8s_api::UnmeshedNetwork> for UnmeshedNetwork {
    fn from(u: linkerd_k8s_api::UnmeshedNetwork) -> Self {
        let name = u.name_unchecked();
        let namespace = u
            .namespace()
            .expect("UnmeshedNetwork must have a namespace");

        UnmeshedNetwork {
            name,
            namespace,
            networks: u.spec.networks.clone(),
            creation_timestamp: u.creation_timestamp().map(|d| d.0),
        }
    }
}

// === impl MatchedUnmeshedNetwork ===

impl std::cmp::PartialOrd for MatchedUnmeshedNetwork {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for MatchedUnmeshedNetwork {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.matched_cidr
            .cmp(&other.matched_cidr)
            .then_with(|| self.creation_timestamp.cmp(&other.creation_timestamp))
            .then_with(|| self.namespace.cmp(&other.namespace).reverse())
            .then_with(|| self.name.cmp(&other.name).reverse())
    }
}

// === impl MatchedCidr ===

impl std::cmp::PartialOrd for MatchedCidr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for MatchedCidr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let cidr_size_self = self.0.block_size();
        let cidr_size_other = other.0.block_size();

        match cidr_size_self.cmp(&cidr_size_other) {
            std::cmp::Ordering::Less => std::cmp::Ordering::Greater,
            std::cmp::Ordering::Greater => std::cmp::Ordering::Less,
            std::cmp::Ordering::Equal => std::cmp::Ordering::Equal,
        }
    }
}

// Attempts to find the best matching network for a certain discovery look-up.
// Logic is:
// 1. if there are Unmeshed networks in the source_namespace, only these are considered
// 2. the target IP is matches against the cidrs of the UnmeshedNetwork
// 3. ambiguity is resolved as:
//    - prefer the more specific cidr match
//    - prefer older resource
//    - if all fails, rely on alphabetical sort of namespace/name
pub(crate) fn resolve_unmeshed_network<'n>(
    addr: IpAddr,
    source_namespace: String,
    nets: impl Iterator<Item = &'n UnmeshedNetwork>,
) -> Option<super::ResourceRef> {
    let (same_ns, rest): (Vec<_>, Vec<_>) = nets.partition(|un| un.namespace == source_namespace);
    let to_pick_from = if !same_ns.is_empty() { same_ns } else { rest };

    to_pick_from
        .iter()
        .filter_map(|unet| {
            let matched_cidr = find_matched_cidr(&unet.networks, addr)?;
            Some(MatchedUnmeshedNetwork {
                name: unet.name.clone(),
                namespace: unet.namespace.clone(),
                matched_cidr,
                creation_timestamp: unet.creation_timestamp,
            })
        })
        .max()
        .map(|m| super::ResourceRef {
            name: m.name,
            namespace: m.namespace,
        })
}

// This finds a CIDR that contains the given IpAddr. When there are
// multiple CIDRS that match this criteria, the CIDR that is most
// specific (as in having the smallest address space) wins.
fn find_matched_cidr(cidrs: &[Cidr], addr: IpAddr) -> Option<MatchedCidr> {
    let ip: Cidr = addr.into();
    cidrs
        .iter()
        .filter(|c| c.contains(&ip))
        .min_by(|a, b| a.block_size().cmp(&b.block_size()))
        .cloned()
        .map(MatchedCidr)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_picks_smallest_cidr() {
        let ip_addr = "192.168.0.4".parse().unwrap();
        let networks = vec![
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "net-1".to_string(),
                namespace: "ns".to_string(),
                creation_timestamp: None,
            },
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/24".parse().unwrap()]),
                name: "net-2".to_string(),
                namespace: "ns".to_string(),
                creation_timestamp: None,
            },
        ];

        let resolved = resolve_unmeshed_network(ip_addr, "ns".into(), networks.iter());
        assert_eq!(resolved.unwrap().name, "net-2".to_string())
    }

    #[test]
    fn test_picks_local_ns() {
        let ip_addr = "192.168.0.4".parse().unwrap();
        let networks = vec![
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "net-1".to_string(),
                namespace: "ns-1".to_string(),
                creation_timestamp: None,
            },
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/24".parse().unwrap()]),
                name: "net-2".to_string(),
                namespace: "ns".to_string(),
                creation_timestamp: None,
            },
        ];

        let resolved = resolve_unmeshed_network(ip_addr, "ns-1".into(), networks.iter());
        assert_eq!(resolved.unwrap().name, "net-1".to_string())
    }

    #[test]
    fn test_picks_older_resource() {
        let ip_addr = "192.168.0.4".parse().unwrap();
        let networks = vec![
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "net-1".to_string(),
                namespace: "ns".to_string(),
                creation_timestamp: Some(DateTime::<Utc>::MAX_UTC),
            },
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "net-2".to_string(),
                namespace: "ns".to_string(),
                creation_timestamp: Some(DateTime::<Utc>::MIN_UTC),
            },
        ];

        let resolved = resolve_unmeshed_network(ip_addr, "ns".into(), networks.iter());
        assert_eq!(resolved.unwrap().name, "net-1".to_string())
    }

    #[test]
    fn test_picks_alphabetical_order() {
        let ip_addr = "192.168.0.4".parse().unwrap();
        let networks = vec![
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "b".to_string(),
                namespace: "a".to_string(),
                creation_timestamp: None,
            },
            UnmeshedNetwork {
                networks: Networks(vec!["192.168.0.1/16".parse().unwrap()]),
                name: "d".to_string(),
                namespace: "c".to_string(),
                creation_timestamp: None,
            },
        ];

        let resolved = resolve_unmeshed_network(ip_addr, "ns".into(), networks.iter());
        assert_eq!(resolved.unwrap().name, "b".to_string())
    }
}
