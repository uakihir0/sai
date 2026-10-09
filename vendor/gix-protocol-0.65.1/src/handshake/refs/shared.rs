use bstr::{BStr, BString, ByteSlice};

use crate::{
    fetch::response::ShallowUpdate,
    handshake::{Ref, refs::parse::Error},
};

const MAX_ADVERTISED_REFS: u64 = 1_000_000;
const MAX_ADVERTISED_REF_NAME_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdvertisementBudget {
    refs: u64,
    name_bytes: u64,
    max_refs: u64,
    max_name_bytes: u64,
}

impl Default for AdvertisementBudget {
    fn default() -> Self {
        Self {
            refs: 0,
            name_bytes: 0,
            max_refs: MAX_ADVERTISED_REFS,
            max_name_bytes: MAX_ADVERTISED_REF_NAME_BYTES,
        }
    }
}

impl AdvertisementBudget {
    #[cfg(test)]
    fn with_limits(max_refs: u64, max_name_bytes: u64) -> Self {
        Self {
            max_refs: max_refs.min(MAX_ADVERTISED_REFS),
            max_name_bytes: max_name_bytes.min(MAX_ADVERTISED_REF_NAME_BYTES),
            ..Self::default()
        }
    }

    pub(in crate::handshake::refs) fn accept_ref_name(&mut self, name: &[u8]) -> Result<(), Error> {
        let refs = self.refs.saturating_add(1);
        if refs > self.max_refs {
            return Err(advertisement_limit_error("ref count", refs));
        }
        self.refs = refs;
        self.accept_ref_name_bytes(name.len())
    }

    pub(in crate::handshake::refs) fn accept_ref_name_bytes(
        &mut self,
        name_bytes: usize,
    ) -> Result<(), Error> {
        let name_bytes = u64::try_from(name_bytes).unwrap_or(u64::MAX);
        let total = self.name_bytes.saturating_add(name_bytes);
        if total > self.max_name_bytes {
            return Err(advertisement_limit_error(
                "cumulative ref-name bytes",
                total,
            ));
        }
        self.name_bytes = total;
        Ok(())
    }
}

fn advertisement_limit_error(limit: &str, actual: u64) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Git ref advertisement exceeded the {limit} limit (reached {actual})"),
    ))
}

pub(in crate::handshake::refs) fn push_v2_ref(
    refs: &mut Vec<Ref>,
    budget: &mut AdvertisementBudget,
    line: &BStr,
) -> Result<(), Error> {
    let ref_ = parse_v2(line)?;
    let (name, _, _) = ref_.unpack();
    let symref_target_bytes = match &ref_ {
        Ref::Symbolic { target, .. } | Ref::Unborn { target, .. } => target.len(),
        Ref::Direct { .. } | Ref::Peeled { .. } => 0,
    };
    budget.accept_ref_name(name.as_bytes())?;
    budget.accept_ref_name_bytes(symref_target_bytes)?;
    refs.push(ref_);
    Ok(())
}

impl From<InternalRef> for Ref {
    fn from(v: InternalRef) -> Self {
        match v {
            InternalRef::Symbolic {
                path,
                target: Some(target),
                tag,
                object,
            } => Ref::Symbolic {
                full_ref_name: path,
                target,
                tag,
                object,
            },
            InternalRef::Symbolic {
                path,
                target: None,
                tag: None,
                object,
            } => Ref::Direct {
                full_ref_name: path,
                object,
            },
            InternalRef::Symbolic {
                path,
                target: None,
                tag: Some(tag),
                object,
            } => Ref::Peeled {
                full_ref_name: path,
                tag,
                object,
            },
            InternalRef::Peeled { path, tag, object } => Ref::Peeled {
                full_ref_name: path,
                tag,
                object,
            },
            InternalRef::Direct { path, object } => Ref::Direct {
                full_ref_name: path,
                object,
            },
            InternalRef::SymbolicForLookup { .. } => {
                unreachable!("this case should have been removed during processing")
            }
        }
    }
}

#[cfg_attr(test, derive(PartialEq, Eq, Debug, Clone))]
pub(crate) enum InternalRef {
    /// A ref pointing to a `tag` object, which in turns points to an `object`, usually a commit
    Peeled {
        path: BString,
        tag: gix_hash::ObjectId,
        object: gix_hash::ObjectId,
    },
    /// A ref pointing to a commit object
    Direct {
        path: BString,
        object: gix_hash::ObjectId,
    },
    /// A symbolic ref pointing to `target` ref, which in turn points to an `object`
    Symbolic {
        path: BString,
        /// It is `None` if the target is unreachable as it points to another namespace than the one is currently set
        /// on the server (i.e. based on the repository at hand or the user performing the operation).
        ///
        /// The latter is more of an edge case, please [this issue][#205] for details.
        target: Option<BString>,
        tag: Option<gix_hash::ObjectId>,
        object: gix_hash::ObjectId,
    },
    /// extracted from V1 capabilities, which contain some important symbolic refs along with their targets
    /// These don't contain the Id
    SymbolicForLookup {
        path: BString,
        target: Option<BString>,
    },
}

impl InternalRef {
    fn unpack_direct(self) -> Option<(BString, gix_hash::ObjectId)> {
        match self {
            InternalRef::Direct { path, object } => Some((path, object)),
            _ => None,
        }
    }
    fn lookup_symbol_has_path(&self, predicate_path: &BStr) -> bool {
        matches!(self, InternalRef::SymbolicForLookup { path, .. } if path == predicate_path)
    }
}

pub(crate) fn from_capabilities<'a>(
    capabilities: impl Iterator<Item = gix_transport::client::capabilities::Capability<'a>>,
) -> Result<(Vec<InternalRef>, AdvertisementBudget), Error> {
    from_capabilities_with_budget(capabilities, AdvertisementBudget::default())
}

fn from_capabilities_with_budget<'a>(
    capabilities: impl Iterator<Item = gix_transport::client::capabilities::Capability<'a>>,
    mut budget: AdvertisementBudget,
) -> Result<(Vec<InternalRef>, AdvertisementBudget), Error> {
    let mut out_refs = Vec::new();
    let symref_values = capabilities.filter_map(|c| {
        if c.name() == b"symref".as_bstr() {
            c.value().map(ToOwned::to_owned)
        } else {
            None
        }
    });
    for symref in symref_values {
        let (left, right) =
            symref.split_at(
                symref
                    .find_byte(b':')
                    .ok_or_else(|| Error::MalformedSymref {
                        symref: symref.to_owned(),
                    })?,
            );
        if left.is_empty() || right.is_empty() {
            return Err(Error::MalformedSymref {
                symref: symref.to_owned(),
            });
        }
        budget.accept_ref_name(left.as_bytes())?;
        let target = &right[1..];
        if target != b"(null)" {
            budget.accept_ref_name_bytes(target.len())?;
        }
        out_refs.push(InternalRef::SymbolicForLookup {
            path: left.into(),
            target: match target {
                b"(null)" => None,
                name => Some(name.into()),
            },
        });
    }
    Ok((out_refs, budget))
}

pub(in crate::handshake::refs) fn parse_v1(
    num_initial_out_refs: usize,
    out_refs: &mut Vec<InternalRef>,
    out_shallow: &mut Vec<ShallowUpdate>,
    budget: &mut AdvertisementBudget,
    line: &BStr,
) -> Result<(), Error> {
    let trimmed = line.trim_end();
    let (hex_hash, path) = trimmed.split_at(
        trimmed
            .find(b" ")
            .ok_or_else(|| Error::MalformedV1RefLine(trimmed.to_owned().into()))?,
    );
    let path = &path[1..];
    if path.is_empty() {
        return Err(Error::MalformedV1RefLine(trimmed.to_owned().into()));
    }
    match path.strip_suffix(b"^{}") {
        Some(stripped) => {
            if hex_hash.iter().all(|b| *b == b'0') && stripped == b"capabilities" {
                // this is a special dummy-ref just for the sake of getting capabilities across in a repo that is empty.
                return Ok(());
            }
            let (previous_path, tag) = out_refs.pop().and_then(InternalRef::unpack_direct).ok_or(
                Error::InvariantViolation {
                    message: "Expecting peeled refs to be preceded by direct refs",
                },
            )?;
            if previous_path != stripped {
                return Err(Error::InvariantViolation {
                    message: "Expecting peeled refs to have the same base path as the previous, unpeeled one",
                });
            }
            out_refs.push(InternalRef::Peeled {
                path: previous_path,
                tag,
                object: gix_hash::ObjectId::from_hex(hex_hash.as_bytes())?,
            });
        }
        None => {
            let object = match gix_hash::ObjectId::from_hex(hex_hash.as_bytes()) {
                Ok(id) => id,
                Err(_) if hex_hash.as_bstr() == "shallow" => {
                    let id = gix_hash::ObjectId::from_hex(path)?;
                    out_shallow.push(ShallowUpdate::Shallow(id));
                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            };
            match out_refs
                .iter()
                .take(num_initial_out_refs)
                .position(|r| r.lookup_symbol_has_path(path.into()))
            {
                Some(position) => {
                    match out_refs.swap_remove(position) {
                        InternalRef::SymbolicForLookup { path: _, target } => {
                            out_refs.push(InternalRef::Symbolic {
                                path: path.into(),
                                tag: None, // TODO: figure out how annotated tags work here.
                                object,
                                target,
                            });
                        }
                        _ => unreachable!(
                            "Bug in lookup_symbol_has_path - must return lookup symbols"
                        ),
                    }
                }
                None => {
                    budget.accept_ref_name(path)?;
                    out_refs.push(InternalRef::Direct {
                        object,
                        path: path.into(),
                    });
                }
            }
        }
    }
    Ok(())
}

pub(in crate::handshake::refs) fn parse_v2(line: &BStr) -> Result<Ref, Error> {
    let trimmed = line.trim_end();
    let mut tokens = trimmed.splitn(4, |b| *b == b' ');
    match (tokens.next(), tokens.next()) {
        (Some(hex_hash), Some(path)) => {
            let id = if hex_hash == b"unborn" {
                None
            } else {
                Some(gix_hash::ObjectId::from_hex(hex_hash.as_bytes())?)
            };
            if path.is_empty() {
                return Err(Error::MalformedV2RefLine(trimmed.to_owned().into()));
            }
            let mut symref_target = None;
            let mut peeled = None;
            for attribute in tokens.by_ref().take(2) {
                let mut tokens = attribute.splitn(2, |b| *b == b':');
                match (tokens.next(), tokens.next()) {
                    (Some(attribute), Some(value)) => {
                        if value.is_empty() {
                            return Err(Error::MalformedV2RefLine(trimmed.to_owned().into()));
                        }
                        match attribute {
                            b"peeled" => {
                                peeled = Some(gix_hash::ObjectId::from_hex(value.as_bytes())?);
                            }
                            b"symref-target" => {
                                symref_target = Some(value);
                            }
                            _ => {
                                return Err(Error::UnknownAttribute {
                                    attribute: attribute.to_owned().into(),
                                    line: trimmed.to_owned().into(),
                                });
                            }
                        }
                    }
                    _ => return Err(Error::MalformedV2RefLine(trimmed.to_owned().into())),
                }
            }
            if tokens.next().is_some() {
                return Err(Error::MalformedV2RefLine(trimmed.to_owned().into()));
            }
            Ok(match (symref_target, peeled) {
                (Some(target_name), peeled) => match target_name {
                    b"(null)" => match peeled {
                        None => Ref::Direct {
                            full_ref_name: path.into(),
                            object: id.ok_or(Error::InvariantViolation {
                                message: "got 'unborn' while (null) was a symref target",
                            })?,
                        },
                        Some(peeled) => Ref::Peeled {
                            full_ref_name: path.into(),
                            object: peeled,
                            tag: id.ok_or(Error::InvariantViolation {
                                message: "got 'unborn' while (null) was a symref target",
                            })?,
                        },
                    },
                    name => match id {
                        Some(id) => Ref::Symbolic {
                            full_ref_name: path.into(),
                            tag: peeled.map(|_| id),
                            object: peeled.unwrap_or(id),
                            target: name.into(),
                        },
                        None => Ref::Unborn {
                            full_ref_name: path.into(),
                            target: name.into(),
                        },
                    },
                },
                (None, Some(peeled)) => Ref::Peeled {
                    full_ref_name: path.into(),
                    object: peeled,
                    tag: id.ok_or(Error::InvariantViolation {
                        message: "got 'unborn' as tag target",
                    })?,
                },
                (None, None) => Ref::Direct {
                    object: id.ok_or(Error::InvariantViolation {
                        message: "got 'unborn' as object name of direct reference",
                    })?,
                    full_ref_name: path.into(),
                },
            })
        }
        _ => Err(Error::MalformedV2RefLine(trimmed.to_owned().into())),
    }
}

#[cfg(test)]
mod tests {
    use gix_transport::client;

    use bstr::ByteSlice;

    use crate::handshake::{
        refs,
        refs::shared::{AdvertisementBudget, InternalRef},
    };

    #[test]
    fn from_capabilities_extracts_symbolic_references() -> Result<(), client::Error> {
        let caps = client::Capabilities::from_bytes(
            b"\0unrelated symref=HEAD:refs/heads/main symref=ANOTHER:refs/heads/foo symref=MISSING_NAMESPACE_TARGET:(null) agent=git/2.28.0",
        )?
            .0;
        let (out, budget) =
            refs::shared::from_capabilities(caps.iter()).expect("a working example");

        assert_eq!(
            out,
            vec![
                InternalRef::SymbolicForLookup {
                    path: "HEAD".into(),
                    target: Some("refs/heads/main".into())
                },
                InternalRef::SymbolicForLookup {
                    path: "ANOTHER".into(),
                    target: Some("refs/heads/foo".into())
                },
                InternalRef::SymbolicForLookup {
                    path: "MISSING_NAMESPACE_TARGET".into(),
                    target: None
                }
            ]
        );
        assert_eq!(budget.refs, 3);
        Ok(())
    }

    #[test]
    fn v1_ref_count_is_rejected_before_growing_the_output() {
        let mut out_refs = Vec::new();
        let mut out_shallow = Vec::new();
        let mut budget = AdvertisementBudget::with_limits(1, 64);

        refs::shared::parse_v1(
            0,
            &mut out_refs,
            &mut out_shallow,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/heads/one".as_bstr(),
        )
        .unwrap();
        let error = refs::shared::parse_v1(
            0,
            &mut out_refs,
            &mut out_shallow,
            &mut budget,
            b"0000000000000000000000000000000000000002 refs/heads/two".as_bstr(),
        )
        .unwrap_err();

        assert_limit_error(error, "ref count", 2);
        assert_eq!(out_refs.len(), 1);
    }

    #[test]
    fn v1_cumulative_ref_name_bytes_are_rejected_before_growing_the_output() {
        let mut out_refs = Vec::new();
        let mut out_shallow = Vec::new();
        let mut budget = AdvertisementBudget::with_limits(10, 3);

        let error = refs::shared::parse_v1(
            0,
            &mut out_refs,
            &mut out_shallow,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/heads/one".as_bstr(),
        )
        .unwrap_err();

        assert_limit_error(
            error,
            "cumulative ref-name bytes",
            b"refs/heads/one".len() as u64,
        );
        assert!(out_refs.is_empty());
    }

    #[test]
    fn v2_ref_count_is_rejected_before_growing_the_output() {
        let mut out_refs = Vec::new();
        let mut budget = AdvertisementBudget::with_limits(1, 64);

        refs::shared::push_v2_ref(
            &mut out_refs,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/tags/v1.0.0".as_bstr(),
        )
        .unwrap();
        let error = refs::shared::push_v2_ref(
            &mut out_refs,
            &mut budget,
            b"0000000000000000000000000000000000000002 refs/tags/v2.0.0".as_bstr(),
        )
        .unwrap_err();

        assert_limit_error(error, "ref count", 2);
        assert_eq!(out_refs.len(), 1);
    }

    #[test]
    fn v2_ref_name_bytes_are_rejected_before_growing_the_output() {
        let mut out_refs = Vec::new();
        let mut budget = AdvertisementBudget::with_limits(10, 3);

        let error = refs::shared::push_v2_ref(
            &mut out_refs,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/tags/v1.0.0".as_bstr(),
        )
        .unwrap_err();

        assert_limit_error(
            error,
            "cumulative ref-name bytes",
            b"refs/tags/v1.0.0".len() as u64,
        );
        assert!(out_refs.is_empty());
    }

    #[test]
    fn v2_symbolic_target_bytes_are_included_in_the_name_budget() {
        let mut out_refs = Vec::new();
        let ref_name = b"refs/heads/HEAD";
        let target = b"refs/heads/main";
        let mut budget =
            AdvertisementBudget::with_limits(10, (ref_name.len() + target.len()) as u64);

        refs::shared::push_v2_ref(
            &mut out_refs,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/heads/HEAD symref-target:refs/heads/main"
                .as_bstr(),
        )
        .unwrap();
        assert_eq!(budget.name_bytes, (ref_name.len() + target.len()) as u64);
        assert_eq!(out_refs.len(), 1);

        let mut out_refs = Vec::new();
        let mut budget =
            AdvertisementBudget::with_limits(10, (ref_name.len() + target.len() - 1) as u64);
        let error = refs::shared::push_v2_ref(
            &mut out_refs,
            &mut budget,
            b"0000000000000000000000000000000000000001 refs/heads/HEAD symref-target:refs/heads/main"
                .as_bstr(),
        )
        .unwrap_err();

        assert_limit_error(
            error,
            "cumulative ref-name bytes",
            (ref_name.len() + target.len()) as u64,
        );
        assert!(out_refs.is_empty());
    }

    #[test]
    fn v1_and_v2_peeled_refs_count_the_primary_name_once() {
        let ref_name = b"refs/tags/v1.0.0";
        let direct = b"0000000000000000000000000000000000000001 refs/tags/v1.0.0";
        let peeled = b"0000000000000000000000000000000000000002 refs/tags/v1.0.0^{}";
        let mut v1_refs = Vec::new();
        let mut v1_shallow = Vec::new();
        let mut v1_budget = AdvertisementBudget::default();

        refs::shared::parse_v1(
            0,
            &mut v1_refs,
            &mut v1_shallow,
            &mut v1_budget,
            direct.as_bstr(),
        )
        .unwrap();
        refs::shared::parse_v1(
            0,
            &mut v1_refs,
            &mut v1_shallow,
            &mut v1_budget,
            peeled.as_bstr(),
        )
        .unwrap();
        assert_eq!(v1_budget.name_bytes, ref_name.len() as u64);

        let mut v2_refs = Vec::new();
        let mut v2_budget = AdvertisementBudget::default();
        refs::shared::push_v2_ref(
            &mut v2_refs,
            &mut v2_budget,
            b"0000000000000000000000000000000000000001 refs/tags/v1.0.0 peeled:0000000000000000000000000000000000000002"
                .as_bstr(),
        )
        .unwrap();
        assert_eq!(v2_budget.name_bytes, ref_name.len() as u64);
    }

    #[test]
    fn v1_symbolic_target_bytes_are_included_in_the_name_budget() -> Result<(), client::Error> {
        let caps = client::Capabilities::from_bytes(b"\0symref=HEAD:refs/heads/main")?.0;
        let ref_name = b"HEAD";
        let target = b"refs/heads/main";
        let (mut out_refs, mut budget) = refs::shared::from_capabilities_with_budget(
            caps.iter(),
            AdvertisementBudget::with_limits(10, (ref_name.len() + target.len()) as u64),
        )
        .expect("symbolic target fits");
        let mut out_shallow = Vec::new();
        refs::shared::parse_v1(
            out_refs.len(),
            &mut out_refs,
            &mut out_shallow,
            &mut budget,
            b"0000000000000000000000000000000000000001 HEAD".as_bstr(),
        )
        .unwrap();
        assert_eq!(budget.name_bytes, (ref_name.len() + target.len()) as u64);

        let error = refs::shared::from_capabilities_with_budget(
            caps.iter(),
            AdvertisementBudget::with_limits(10, (ref_name.len() + target.len() - 1) as u64),
        )
        .unwrap_err();
        assert_limit_error(
            error,
            "cumulative ref-name bytes",
            (ref_name.len() + target.len()) as u64,
        );
        Ok(())
    }

    fn assert_limit_error(error: refs::parse::Error, limit: &str, actual: u64) {
        match error {
            refs::parse::Error::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
                assert_eq!(
                    error.to_string(),
                    format!("Git ref advertisement exceeded the {limit} limit (reached {actual})")
                );
            }
            other => panic!("expected an invalid-data limit error, got {other:?}"),
        }
    }
}
