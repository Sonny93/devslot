//! Reading the registry document into records, and applying one record to the
//! document in place so everything else in the file stays as it is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, value};

use super::RegistryError;
use super::record::{
    AssignedPort, CheckoutOrigin, InstanceRecord, PortDetail, Registry, RegistryChange,
    normalize_path,
};
use crate::discovery::PortKind;
use crate::global_config::MAX_SLOT_SIZE;
use crate::host_label::{host_of_label, is_valid_host, pick_label};
use crate::port_range::PortRange;
use crate::project_config::{check_variable_name, is_valid_variable_name};

/// The only registry version devslot understands.
pub const REGISTRY_VERSION: i64 = 1;

/// The text of a registry with no instance.
pub const EMPTY_REGISTRY: &str = "version = 1\n";

const VERSION_KEY: &str = "version";
const OPTED_OUT_KEY: &str = "opted_out";
const INSTANCE_KEY: &str = "instance";
const HOST_KEY: &str = "host";
const WEB_PORT_KEY: &str = "web_port";
const PROJECT_KEY: &str = "project";
const WORKTREE_KEY: &str = "worktree";

struct Problem {
    entry: String,
    reason: String,
}

fn problem(entry: &str, reason: impl Into<String>) -> Problem {
    Problem {
        entry: entry.to_owned(),
        reason: reason.into(),
    }
}

/// Parses `text`, the content of the registry file at `file`.
pub fn parse_registry(text: &str, file: &Path) -> Result<(DocumentMut, Registry), RegistryError> {
    let document: DocumentMut =
        text.parse()
            .map_err(|error: toml_edit::TomlError| RegistryError::Parse {
                path: file.to_path_buf(),
                message: error.message().to_owned(),
            })?;
    let registry = read_registry(&document).map_err(|found| RegistryError::Malformed {
        path: file.to_path_buf(),
        entry: found.entry,
        reason: found.reason,
    })?;
    Ok((document, registry))
}

fn read_registry(document: &DocumentMut) -> Result<Registry, Problem> {
    check_version(document)?;
    let opted_out = read_opted_out(document)?;
    let mut instances = read_instances(document)?;
    fill_missing_hosts(&mut instances);
    check_consistency(&instances)?;
    Ok(Registry {
        instances,
        opted_out,
    })
}

fn check_version(document: &DocumentMut) -> Result<(), Problem> {
    match document.get(VERSION_KEY).and_then(Item::as_integer) {
        Some(REGISTRY_VERSION) => Ok(()),
        Some(other) => Err(problem(
            VERSION_KEY,
            format!("version {other} is not supported, expected {REGISTRY_VERSION}"),
        )),
        None => Err(problem(VERSION_KEY, "missing or not an integer")),
    }
}

fn read_opted_out(document: &DocumentMut) -> Result<Vec<PathBuf>, Problem> {
    let Some(item) = document.get(OPTED_OUT_KEY) else {
        return Ok(Vec::new());
    };
    let array = item
        .as_array()
        .ok_or_else(|| problem(OPTED_OUT_KEY, "must be a list of absolute paths"))?;
    array
        .iter()
        .map(|entry| {
            let path = entry
                .as_str()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute());
            path.ok_or_else(|| problem(OPTED_OUT_KEY, "must be a list of absolute paths"))
        })
        .collect()
}

fn read_instances(document: &DocumentMut) -> Result<Vec<InstanceRecord>, Problem> {
    let Some(item) = document.get(INSTANCE_KEY) else {
        return Ok(Vec::new());
    };
    let tables = item
        .as_array_of_tables()
        .ok_or_else(|| problem(INSTANCE_KEY, "must be a list of [[instance]] tables"))?;
    tables
        .iter()
        .enumerate()
        .map(|(index, table)| read_instance(table, index))
        .collect()
}

fn read_instance(table: &Table, index: usize) -> Result<InstanceRecord, Problem> {
    let entry = entry_label(table, index);
    let path = PathBuf::from(string_field(table, "path", &entry)?);
    if !path.is_absolute() {
        return Err(problem(&entry, "path is not absolute"));
    }
    let name = string_field(table, "name", &entry)?.to_owned();
    if name.is_empty() {
        return Err(problem(&entry, "name is empty"));
    }
    let slot = integer_field(table, "slot", &entry)?;
    let slot = u32::try_from(slot).map_err(|_| problem(&entry, "slot is out of range"))?;
    let range = read_range(table.get("range"), &entry)?;
    let ports = read_ports(table, range, &entry)?;
    let host = read_host(table, &entry)?;
    let web_port_variable = read_web_port_variable(table, &entry)?;
    Ok(InstanceRecord {
        path,
        name,
        slot,
        range,
        ports,
        is_host_unsaved: host.is_none(),
        host: host.unwrap_or_default(),
        web_port_variable,
        origin: read_origin(table),
    })
}

/// Reads the origin leniently: it is cosmetic, so anything but a non-empty
/// `project` string with a `worktree` bool reads as absent, never as malformed.
fn read_origin(table: &Table) -> Option<CheckoutOrigin> {
    let project = table
        .get(PROJECT_KEY)
        .and_then(Item::as_str)
        .filter(|project| !project.is_empty())?;
    let is_worktree = table.get(WORKTREE_KEY).and_then(Item::as_bool)?;
    Some(CheckoutOrigin {
        project: project.to_owned(),
        is_worktree,
    })
}

fn read_host(table: &Table, entry: &str) -> Result<Option<String>, Problem> {
    let Some(item) = table.get(HOST_KEY) else {
        return Ok(None);
    };
    let host = item
        .as_str()
        .filter(|host| is_valid_host(host))
        .ok_or_else(|| {
            problem(
                entry,
                "host must be one <label>.localhost name of letters, digits and dashes",
            )
        })?;
    Ok(Some(host.to_owned()))
}

fn read_web_port_variable(table: &Table, entry: &str) -> Result<Option<String>, Problem> {
    let Some(item) = table.get(WEB_PORT_KEY) else {
        return Ok(None);
    };
    let variable = item
        .as_str()
        .filter(|variable| is_valid_variable_name(variable))
        .ok_or_else(|| problem(entry, "web_port must be a variable name"))?;
    Ok(Some(variable.to_owned()))
}

/// Gives every entry without a host the one a registration would have given
/// it, in file order, around the hosts already stored.
fn fill_missing_hosts(instances: &mut [InstanceRecord]) {
    let mut taken: HashSet<String> = instances
        .iter()
        .filter(|instance| !instance.is_host_unsaved)
        .map(|instance| instance.host.clone())
        .collect();
    for instance in instances
        .iter_mut()
        .filter(|instance| instance.is_host_unsaved)
    {
        let label = pick_label(&instance.name, instance.slot, |label| {
            taken.contains(&host_of_label(label))
        });
        instance.host = host_of_label(&label);
        taken.insert(instance.host.clone());
    }
}

fn entry_label(table: &Table, index: usize) -> String {
    match table.get("path").and_then(Item::as_str) {
        Some(path) => format!("instance #{} ({path})", index + 1),
        None => format!("instance #{}", index + 1),
    }
}

fn string_field<'a>(table: &'a Table, key: &str, entry: &str) -> Result<&'a str, Problem> {
    table
        .get(key)
        .and_then(Item::as_str)
        .ok_or_else(|| problem(entry, format!("{key} is missing or not a string")))
}

fn integer_field(table: &Table, key: &str, entry: &str) -> Result<i64, Problem> {
    table
        .get(key)
        .and_then(Item::as_integer)
        .ok_or_else(|| problem(entry, format!("{key} is missing or not an integer")))
}

fn read_range(item: Option<&Item>, entry: &str) -> Result<PortRange, Problem> {
    let numbers: Option<Vec<u16>> = item.and_then(Item::as_array).and_then(|array| {
        array
            .iter()
            .map(|number| number.as_integer().and_then(|n| u16::try_from(n).ok()))
            .collect()
    });
    let Some([first, last]) = numbers.as_deref() else {
        return Err(problem(entry, "range must be [first, last] with two ports"));
    };
    let range = PortRange {
        first: *first,
        last: *last,
    };
    if range.is_empty() {
        return Err(problem(entry, "range is reversed"));
    }
    if range.len() > usize::from(MAX_SLOT_SIZE) {
        return Err(problem(
            entry,
            format!("range is longer than {MAX_SLOT_SIZE} ports"),
        ));
    }
    Ok(range)
}

fn read_ports(table: &Table, range: PortRange, entry: &str) -> Result<Vec<AssignedPort>, Problem> {
    let ports = table_like_field(table, "ports", entry)?;
    let details = table_like_field(table, "details", entry)?;
    for (variable, _) in ports.iter().chain(details.iter()) {
        if let Err(reason) = check_variable_name(variable) {
            return Err(problem(entry, format!("variable {variable:?}: {reason}")));
        }
    }
    for (variable, _) in details.iter() {
        if !ports.contains_key(variable) {
            return Err(problem(entry, format!("details of {variable} has no port")));
        }
    }
    ports
        .iter()
        .map(|(variable, item)| {
            let port = item
                .as_integer()
                .and_then(|number| u16::try_from(number).ok())
                .ok_or_else(|| problem(entry, format!("port of {variable} is not a valid port")))?;
            if !range.contains(port) {
                return Err(problem(
                    entry,
                    format!("port of {variable} is outside the range {range}"),
                ));
            }
            let detail_item = details
                .get(variable)
                .ok_or_else(|| problem(entry, format!("{variable} has no details")))?;
            Ok(AssignedPort {
                variable: variable.to_owned(),
                port,
                detail: read_detail(detail_item, variable, entry)?,
            })
        })
        .collect()
}

fn table_like_field<'a>(
    table: &'a Table,
    key: &str,
    entry: &str,
) -> Result<&'a dyn TableLike, Problem> {
    table
        .get(key)
        .and_then(Item::as_table_like)
        .ok_or_else(|| problem(entry, format!("{key} is missing or not a table")))
}

fn read_detail(item: &Item, variable: &str, entry: &str) -> Result<PortDetail, Problem> {
    let invalid = |what: &str| problem(entry, format!("details of {variable}: {what}"));
    let detail = item.as_table_like().ok_or_else(|| invalid("not a table"))?;
    let kind = detail
        .get("kind")
        .and_then(Item::as_str)
        .and_then(PortKind::from_word)
        .ok_or_else(|| invalid("kind must be \"web\" or \"service\""))?;
    let service = match detail.get("service") {
        None => None,
        Some(item) => Some(
            item.as_str()
                .ok_or_else(|| invalid("service is not a string"))?
                .to_owned(),
        ),
    };
    let target = match detail.get("target") {
        None => None,
        Some(item) => Some(
            item.as_integer()
                .and_then(|number| u16::try_from(number).ok())
                .ok_or_else(|| invalid("target is not a valid port"))?,
        ),
    };
    Ok(PortDetail {
        kind,
        service,
        target,
    })
}

fn check_consistency(instances: &[InstanceRecord]) -> Result<(), Problem> {
    if let Some((first, second)) = first_duplicate_path(instances) {
        return Err(pair_problem(
            instances,
            first,
            second,
            "the same path is registered twice",
        ));
    }
    if let Some((first, second)) = first_duplicate_name(instances) {
        let reason = format!("the name {} is used twice", instances[first].name);
        return Err(pair_problem(instances, first, second, reason));
    }
    if let Some((first, second)) = first_duplicate_host(instances) {
        let reason = format!("the host {} is used twice", instances[first].host);
        return Err(pair_problem(instances, first, second, reason));
    }
    if let Some((first, second)) = first_overlapping_range(instances) {
        return Err(pair_problem(
            instances,
            first,
            second,
            "their ranges overlap",
        ));
    }
    if let Some((first, second, port)) = first_shared_port(instances) {
        let reason = format!("both hold port {port}");
        return Err(pair_problem(instances, first, second, reason));
    }
    Ok(())
}

fn pair_problem(
    instances: &[InstanceRecord],
    first: usize,
    second: usize,
    reason: impl Into<String>,
) -> Problem {
    let entry = format!(
        "instance {} and instance {}",
        instances[first].path.display(),
        instances[second].path.display()
    );
    problem(&entry, reason)
}

fn first_duplicate_path(instances: &[InstanceRecord]) -> Option<(usize, usize)> {
    let mut seen: HashMap<PathBuf, usize> = HashMap::with_capacity(instances.len());
    instances.iter().enumerate().find_map(|(index, instance)| {
        seen.insert(normalize_path(&instance.path), index)
            .map(|earlier| (earlier, index))
    })
}

fn first_duplicate_name(instances: &[InstanceRecord]) -> Option<(usize, usize)> {
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(instances.len());
    instances.iter().enumerate().find_map(|(index, instance)| {
        seen.insert(instance.name.as_str(), index)
            .map(|earlier| (earlier, index))
    })
}

fn first_duplicate_host(instances: &[InstanceRecord]) -> Option<(usize, usize)> {
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(instances.len());
    instances.iter().enumerate().find_map(|(index, instance)| {
        seen.insert(instance.host.as_str(), index)
            .map(|earlier| (earlier, index))
    })
}

fn first_overlapping_range(instances: &[InstanceRecord]) -> Option<(usize, usize)> {
    let mut order: Vec<usize> = (0..instances.len()).collect();
    order.sort_by_key(|index| (instances[*index].range.first, *index));
    let mut widest: Option<usize> = None;
    for index in order {
        let range = instances[index].range;
        if let Some(reaching) = widest.filter(|reaching| instances[*reaching].range.overlaps(range))
        {
            return Some((reaching.min(index), reaching.max(index)));
        }
        if widest.is_none_or(|reaching| range.last > instances[reaching].range.last) {
            widest = Some(index);
        }
    }
    None
}

fn first_shared_port(instances: &[InstanceRecord]) -> Option<(usize, usize, u16)> {
    let mut holders: HashMap<u16, usize> = HashMap::new();
    for (index, instance) in instances.iter().enumerate() {
        for assigned in &instance.ports {
            match holders.get(&assigned.port) {
                Some(holder) if *holder != index => {
                    return Some((*holder, index, assigned.port));
                }
                Some(_) => {}
                None => {
                    holders.insert(assigned.port, index);
                }
            }
        }
    }
    None
}

/// Sets `record` in the document: the table of its path is updated in place
/// (only the values that changed are touched), or a new one is appended.
pub fn apply_record(
    document: &mut DocumentMut,
    record: &InstanceRecord,
    file: &Path,
) -> Result<(), RegistryError> {
    let path_text = record
        .path
        .to_str()
        .ok_or_else(|| RegistryError::PathNotUtf8 {
            path: record.path.clone(),
        })?;
    let ends_with_blank_line = document.to_string().ends_with("\n\n");
    let tables = instance_tables(document, file)?;
    let wanted = normalize_path(&record.path);
    let position = tables.iter().position(|table| {
        table
            .get("path")
            .and_then(Item::as_str)
            .is_some_and(|path| normalize_path(Path::new(path)) == wanted)
    });
    match position.and_then(|index| tables.get_mut(index)) {
        Some(table) => update_table(table, record),
        None => tables.push(new_table(record, path_text, !ends_with_blank_line)),
    }
    Ok(())
}

/// Applies one change to the document in place.
pub fn apply_change(
    document: &mut DocumentMut,
    change: &RegistryChange,
    file: &Path,
) -> Result<(), RegistryError> {
    match change {
        RegistryChange::SaveInstance(record) => apply_record(document, record, file),
        RegistryChange::RemoveInstance(path) => remove_record(document, path, file),
        RegistryChange::OptOut(path) => add_opted_out(document, path, file),
        RegistryChange::ClearOptOut(path) | RegistryChange::ForgetOptOut(path) => {
            remove_opted_out(document, path, file)
        }
    }
}

fn remove_record(
    document: &mut DocumentMut,
    path: &Path,
    file: &Path,
) -> Result<(), RegistryError> {
    if document.get(INSTANCE_KEY).is_none() {
        return Ok(());
    }
    let tables = instance_tables(document, file)?;
    let wanted = normalize_path(path);
    let position = tables.iter().position(|table| {
        table
            .get("path")
            .and_then(Item::as_str)
            .is_some_and(|existing| normalize_path(Path::new(existing)) == wanted)
    });
    if let Some(index) = position {
        tables.remove(index);
    }
    Ok(())
}

fn add_opted_out(
    document: &mut DocumentMut,
    path: &Path,
    file: &Path,
) -> Result<(), RegistryError> {
    let path_text = path.to_str().ok_or_else(|| RegistryError::PathNotUtf8 {
        path: path.to_path_buf(),
    })?;
    let list = opted_out_list(document, file)?;
    if position_in(list, path).is_none() {
        list.push(path_text);
    }
    Ok(())
}

fn remove_opted_out(
    document: &mut DocumentMut,
    path: &Path,
    file: &Path,
) -> Result<(), RegistryError> {
    if document.get(OPTED_OUT_KEY).is_none() {
        return Ok(());
    }
    let list = opted_out_list(document, file)?;
    if let Some(index) = position_in(list, path) {
        list.remove(index);
    }
    Ok(())
}

fn position_in(list: &Array, path: &Path) -> Option<usize> {
    let wanted = normalize_path(path);
    list.iter().position(|entry| {
        entry
            .as_str()
            .is_some_and(|existing| normalize_path(Path::new(existing)) == wanted)
    })
}

fn opted_out_list<'a>(
    document: &'a mut DocumentMut,
    file: &Path,
) -> Result<&'a mut Array, RegistryError> {
    document
        .entry(OPTED_OUT_KEY)
        .or_insert_with(|| value(Array::new()))
        .as_array_mut()
        .ok_or_else(|| RegistryError::Malformed {
            path: file.to_path_buf(),
            entry: OPTED_OUT_KEY.to_owned(),
            reason: "must be a list of absolute paths".to_owned(),
        })
}

fn instance_tables<'a>(
    document: &'a mut DocumentMut,
    file: &Path,
) -> Result<&'a mut ArrayOfTables, RegistryError> {
    document
        .entry(INSTANCE_KEY)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| RegistryError::Malformed {
            path: file.to_path_buf(),
            entry: INSTANCE_KEY.to_owned(),
            reason: "must be a list of [[instance]] tables".to_owned(),
        })
}

fn new_table(record: &InstanceRecord, path_text: &str, needs_blank_line: bool) -> Table {
    let mut table = Table::new();
    if needs_blank_line {
        table.decor_mut().set_prefix("\n");
    }
    table.insert("path", value(path_text));
    table.insert("name", value(record.name.as_str()));
    table.insert(HOST_KEY, value(record.host.as_str()));
    table.insert("slot", value(i64::from(record.slot)));
    table.insert("range", value(range_array(record.range)));
    let mut ports = InlineTable::new();
    let mut details = InlineTable::new();
    for assigned in &record.ports {
        ports.insert(&assigned.variable, i64::from(assigned.port).into());
        details.insert(&assigned.variable, detail_value(&assigned.detail));
    }
    table.insert("ports", value(ports));
    table.insert("details", value(details));
    if let Some(variable) = &record.web_port_variable {
        table.insert(WEB_PORT_KEY, value(variable.as_str()));
    }
    if let Some(origin) = &record.origin {
        table.insert(PROJECT_KEY, value(origin.project.as_str()));
        table.insert(WORKTREE_KEY, value(origin.is_worktree));
    }
    table
}

fn range_array(range: PortRange) -> Array {
    [range.first, range.last]
        .into_iter()
        .map(i64::from)
        .collect()
}

fn detail_value(detail: &PortDetail) -> toml_edit::Value {
    let mut table = InlineTable::new();
    table.insert("kind", detail.kind.word().into());
    if let Some(service) = &detail.service {
        table.insert("service", service.as_str().into());
    }
    if let Some(target) = detail.target {
        table.insert("target", i64::from(target).into());
    }
    table.into()
}

fn update_table(table: &mut Table, record: &InstanceRecord) {
    let entry = record.path.display().to_string();
    for assigned in &record.ports {
        set_port(table, assigned);
        set_detail(table, assigned, &entry);
    }
    if table.get(HOST_KEY).is_none() {
        table.insert(HOST_KEY, value(record.host.as_str()));
    }
    set_web_port_variable(table, record.web_port_variable.as_deref());
    if let Some(origin) = &record.origin {
        set_origin(table, origin);
    }
}

fn set_origin(table: &mut Table, origin: &CheckoutOrigin) {
    if read_origin(table).as_ref() == Some(origin) {
        return;
    }
    table.insert(PROJECT_KEY, value(origin.project.as_str()));
    table.insert(WORKTREE_KEY, value(origin.is_worktree));
}

fn set_web_port_variable(table: &mut Table, variable: Option<&str>) {
    let current = table.get(WEB_PORT_KEY).and_then(Item::as_str);
    if current == variable {
        return;
    }
    match variable {
        Some(variable) => {
            table.insert(WEB_PORT_KEY, value(variable));
        }
        None => {
            table.remove(WEB_PORT_KEY);
        }
    }
}

fn set_port(table: &mut Table, assigned: &AssignedPort) {
    let Some(ports) = table.get_mut("ports").and_then(Item::as_table_like_mut) else {
        return;
    };
    let wanted = i64::from(assigned.port);
    if ports.get(&assigned.variable).and_then(Item::as_integer) == Some(wanted) {
        return;
    }
    ports.insert(&assigned.variable, value(wanted));
    tidy_inline_table(table, "ports");
}

fn tidy_inline_table(table: &mut Table, key: &str) {
    if let Some(inline) = table.get_mut(key).and_then(Item::as_inline_table_mut) {
        inline.fmt();
    }
}

fn set_detail(table: &mut Table, assigned: &AssignedPort, entry: &str) {
    let Some(details) = table.get_mut("details").and_then(Item::as_table_like_mut) else {
        return;
    };
    let is_current = details
        .get(&assigned.variable)
        .and_then(|item| read_detail(item, &assigned.variable, entry).ok())
        .is_some_and(|current| current == assigned.detail);
    if is_current {
        return;
    }
    details.insert(&assigned.variable, value(detail_value(&assigned.detail)));
    tidy_inline_table(table, "details");
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const FILE: &str = "/state/registry.toml";

    const SPEC_EXAMPLE: &str = r#"version = 1

[[instance]]
path = "/work/my-links.feat-auth"
name = "my-links-feat-auth-3f2a"
host = "my-links-feat-auth-3f2a.localhost"
slot = 1
range = [20050, 20099]
ports = { PORT = 20050, VITE_HMR_PORT = 20051, DB_PORT = 20052 }
details = { PORT = { kind = "web" }, VITE_HMR_PORT = { kind = "web" }, DB_PORT = { kind = "service", service = "postgres", target = 5432 } }
"#;

    fn parse(text: &str) -> Result<Registry, RegistryError> {
        parse_registry(text, Path::new(FILE)).map(|(_, registry)| registry)
    }

    fn malformed_reason(text: &str) -> String {
        match parse(text).unwrap_err() {
            RegistryError::Malformed { entry, reason, .. } => format!("{entry}: {reason}"),
            other => panic!("unexpected error {other}"),
        }
    }

    fn entry(path: &str, name: &str, range: (u16, u16), port: u32) -> String {
        format!(
            "\n[[instance]]\npath = \"{path}\"\nname = \"{name}\"\nhost = \"{name}.localhost\"\nslot = 1\nrange = [{}, {}]\nports = {{ PORT = {port} }}\ndetails = {{ PORT = {{ kind = \"web\" }} }}\n",
            range.0, range.1
        )
    }

    fn registry_text(entries: &[String]) -> String {
        format!("version = 1\n{}", entries.concat())
    }

    fn web_port(variable: &str, port: u16) -> AssignedPort {
        AssignedPort {
            variable: variable.to_owned(),
            port,
            detail: PortDetail {
                kind: PortKind::Web,
                service: None,
                target: None,
            },
        }
    }

    fn record(
        path: &str,
        name: &str,
        range: (u16, u16),
        ports: Vec<AssignedPort>,
    ) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from(path),
            name: name.to_owned(),
            slot: 1,
            range: PortRange {
                first: range.0,
                last: range.1,
            },
            ports,
            host: format!("{name}.localhost"),
            web_port_variable: None,
            origin: None,
            is_host_unsaved: false,
        }
    }

    fn origin(project: &str, is_worktree: bool) -> CheckoutOrigin {
        CheckoutOrigin {
            project: project.to_owned(),
            is_worktree,
        }
    }

    fn apply(text: &str, record: &InstanceRecord) -> String {
        let (mut document, _) = parse_registry(text, Path::new(FILE)).unwrap();
        apply_record(&mut document, record, Path::new(FILE)).unwrap();
        document.to_string()
    }

    #[test]
    fn should_read_the_spec_example_with_its_details() {
        let registry = parse(SPEC_EXAMPLE).unwrap();

        let instance = &registry.instances[0];
        assert_eq!(
            (
                instance.name.as_str(),
                instance.slot,
                instance.range,
                instance.ports.len(),
                instance.ports[2].detail.service.as_deref(),
                instance.ports[2].detail.target
            ),
            (
                "my-links-feat-auth-3f2a",
                1,
                PortRange {
                    first: 20050,
                    last: 20099
                },
                3,
                Some("postgres"),
                Some(5432)
            )
        );
    }

    #[test]
    fn should_keep_the_order_of_the_ports_as_written() {
        let registry = parse(SPEC_EXAMPLE).unwrap();

        let variables: Vec<&str> = registry.instances[0]
            .ports
            .iter()
            .map(|assigned| assigned.variable.as_str())
            .collect();
        assert_eq!(variables, ["PORT", "VITE_HMR_PORT", "DB_PORT"]);
    }

    #[test]
    fn should_read_an_empty_registry_when_it_only_has_a_version() {
        assert_eq!(parse("version = 1\n").unwrap(), Registry::default());
    }

    #[test]
    fn should_accept_the_opted_out_list_when_it_holds_absolute_paths() {
        let registry = parse("version = 1\nopted_out = [\"/work/a\"]\n").unwrap();

        assert_eq!(registry.opted_out, vec![PathBuf::from("/work/a")]);
    }

    #[test]
    fn should_refuse_an_opted_out_list_with_a_relative_path() {
        assert!(malformed_reason("version = 1\nopted_out = [\"a\"]\n").starts_with("opted_out"));
    }

    #[test]
    fn should_refuse_a_registry_without_a_version() {
        assert!(malformed_reason("").starts_with("version"));
    }

    #[test]
    fn should_refuse_a_registry_of_another_version() {
        assert!(malformed_reason("version = 2\n").contains("version 2 is not supported"));
    }

    #[test]
    fn should_report_a_parse_error_when_the_text_is_not_toml() {
        assert!(matches!(
            parse("version = [\n").unwrap_err(),
            RegistryError::Parse { .. }
        ));
    }

    #[test]
    fn should_refuse_an_entry_with_a_relative_path_naming_the_entry() {
        let text = registry_text(&[entry("work/a", "a", (20050, 20099), 20050)]);

        assert!(malformed_reason(&text).contains("instance #1 (work/a): path is not absolute"));
    }

    #[test]
    fn should_refuse_two_entries_with_the_same_path_once_normalized() {
        let text = registry_text(&[
            entry("/work/a", "a", (20050, 20099), 20050),
            entry("/work/a/", "b", (20100, 20149), 20100),
        ]);

        assert!(malformed_reason(&text).contains("the same path is registered twice"));
    }

    #[test]
    fn should_refuse_two_entries_with_the_same_name() {
        let text = registry_text(&[
            entry("/work/a", "same", (20050, 20099), 20050),
            entry("/work/b", "same", (20100, 20149), 20100),
        ]);

        assert!(malformed_reason(&text).contains("the name same is used twice"));
    }

    #[test]
    fn should_refuse_two_entries_with_overlapping_ranges() {
        let text = registry_text(&[
            entry("/work/a", "a", (20050, 20099), 20050),
            entry("/work/b", "b", (20099, 20149), 20100),
        ]);

        assert!(malformed_reason(&text).contains("their ranges overlap"));
    }

    #[test]
    fn should_refuse_a_range_nested_in_a_wider_range_of_a_non_adjacent_entry() {
        let text = registry_text(&[
            entry("/work/a", "a", (20000, 20099), 20000),
            entry("/work/b", "b", (20100, 20149), 20100),
            entry("/work/c", "c", (20050, 20059), 20050),
        ]);

        let reason = malformed_reason(&text);

        assert!(reason.contains("instance /work/a and instance /work/c"));
        assert!(reason.contains("their ranges overlap"));
    }

    #[test]
    fn should_accept_a_large_registry_whose_entries_do_not_conflict() {
        let entries: Vec<String> = (0..2000_u16)
            .map(|index| {
                let first = 1100 + index * 10;
                entry(
                    &format!("/work/{index}"),
                    &format!("n{index}"),
                    (first, first + 9),
                    u32::from(first),
                )
            })
            .collect();

        assert!(parse(&registry_text(&entries)).is_ok());
    }

    #[test]
    fn should_refuse_a_port_outside_the_range_of_its_instance() {
        let text = registry_text(&[entry("/work/a", "a", (20050, 20099), 20100)]);

        assert!(malformed_reason(&text).contains("port of PORT is outside the range 20050-20099"));
    }

    #[test]
    fn should_refuse_a_range_longer_than_100_ports() {
        let text = registry_text(&[entry("/work/a", "a", (20000, 20100), 20000)]);

        assert!(malformed_reason(&text).contains("longer than 100"));
    }

    #[test]
    fn should_refuse_a_reversed_range() {
        let text = registry_text(&[entry("/work/a", "a", (20099, 20050), 20060)]);

        assert!(malformed_reason(&text).contains("range is reversed"));
    }

    #[test]
    fn should_refuse_a_port_outside_u16() {
        let text = registry_text(&[entry("/work/a", "a", (20050, 20099), 70000)]);

        assert!(malformed_reason(&text).contains("port of PORT is not a valid port"));
    }

    #[test]
    fn should_refuse_a_port_without_details() {
        let text = "version = 1\n\n[[instance]]\npath = \"/a\"\nname = \"a\"\nslot = 1\nrange = [20050, 20099]\nports = { PORT = 20050 }\ndetails = {}\n";

        assert!(malformed_reason(text).contains("PORT has no details"));
    }

    #[test]
    fn should_refuse_a_port_variable_name_that_carries_shell_code() {
        let text = SPEC_EXAMPLE
            .replace("{ PORT = 20050,", "{ \"A;id\" = 20050,")
            .replace("details = { PORT =", "details = { \"A;id\" =");

        assert!(malformed_reason(&text).contains("A;id"));
    }

    #[test]
    fn should_refuse_a_reserved_port_variable_name() {
        let text = SPEC_EXAMPLE
            .replace("{ PORT = 20050,", "{ PATH = 20050,")
            .replace("details = { PORT =", "details = { PATH =");

        assert!(malformed_reason(&text).contains("PATH"));
    }

    #[test]
    fn should_refuse_a_kind_that_is_neither_web_nor_service() {
        let text = SPEC_EXAMPLE.replace("kind = \"service\"", "kind = \"db\"");

        assert!(malformed_reason(&text).contains("kind must be"));
    }

    #[test]
    fn should_write_a_new_entry_in_the_documented_format_when_the_registry_is_empty() {
        let new = record(
            "/work/my-links.feat-auth",
            "my-links-feat-auth-3f2a",
            (20050, 20099),
            vec![
                web_port("PORT", 20050),
                web_port("VITE_HMR_PORT", 20051),
                AssignedPort {
                    variable: "DB_PORT".to_owned(),
                    port: 20052,
                    detail: PortDetail {
                        kind: PortKind::Service,
                        service: Some("postgres".to_owned()),
                        target: Some(5432),
                    },
                },
            ],
        );

        assert_eq!(apply(EMPTY_REGISTRY, &new), SPEC_EXAMPLE);
    }

    #[test]
    fn should_append_a_new_entry_after_the_existing_text_without_touching_it() {
        let before = format!("# my notes\n{SPEC_EXAMPLE}\n# trailing comment\n");
        let new = record(
            "/work/b",
            "b",
            (20100, 20149),
            vec![web_port("PORT", 20100)],
        );

        let after = apply(&before, &new);

        assert!(after.starts_with("# my notes\nversion = 1\n"));
        assert!(after.contains(SPEC_EXAMPLE.trim_start_matches("version = 1\n")));
        assert!(after.ends_with("# trailing comment\n"));
        assert!(parse(&after).unwrap().find(Path::new("/work/b")).is_some());
    }

    #[test]
    fn should_leave_the_text_byte_for_byte_when_the_record_is_already_there() {
        let existing = parse(SPEC_EXAMPLE).unwrap().instances[0].clone();

        assert_eq!(apply(SPEC_EXAMPLE, &existing), SPEC_EXAMPLE);
    }

    #[test]
    fn should_update_only_the_matching_entry_when_a_variable_is_added() {
        let first = entry("/work/a", "a", (20050, 20099), 20050);
        let second = entry("/work/b", "b", (20100, 20149), 20100);
        let third = entry("/work/c", "c", (20150, 20199), 20150);
        let before = format!(
            "# header comment\nversion = 1\nunknown_key = \"kept\"\n{first}# between a and b\n{second}{third}# tail\n"
        );
        let mut updated = parse(&before).unwrap().instances[1].clone();
        updated.ports.push(web_port("DB_PORT", 20101));

        let after = apply(&before, &updated);

        let expected_second = second
            .replace("{ PORT = 20100 }", "{ PORT = 20100, DB_PORT = 20101 }")
            .replace(
                "{ PORT = { kind = \"web\" } }",
                "{ PORT = { kind = \"web\" }, DB_PORT = { kind = \"web\" } }",
            );
        assert_eq!(
            after,
            format!(
                "# header comment\nversion = 1\nunknown_key = \"kept\"\n{first}# between a and b\n{expected_second}{third}# tail\n"
            )
        );
    }

    #[test]
    fn should_keep_a_comment_inside_the_updated_entry() {
        let before = SPEC_EXAMPLE.replace("slot = 1\n", "slot = 1 # hand written note\n");
        let mut updated = parse(&before).unwrap().instances[0].clone();
        updated.ports.push(web_port("MAIL_PORT", 20053));

        let after = apply(&before, &updated);

        assert!(after.contains("slot = 1 # hand written note\n"));
        assert!(after.contains("MAIL_PORT = 20053"));
    }

    #[test]
    fn should_update_the_details_of_a_variable_without_touching_its_port() {
        let mut updated = parse(SPEC_EXAMPLE).unwrap().instances[0].clone();
        updated.ports[0].detail.service = Some("web".to_owned());

        let after = apply(SPEC_EXAMPLE, &updated);

        assert!(after.contains("PORT = { kind = \"web\", service = \"web\" }"));
        assert!(after.contains("ports = { PORT = 20050, VITE_HMR_PORT = 20051, DB_PORT = 20052 }"));
    }

    #[test]
    fn should_match_the_entry_by_normalized_path_when_the_record_path_has_a_trailing_slash() {
        let mut updated = parse(SPEC_EXAMPLE).unwrap().instances[0].clone();
        updated.path = PathBuf::from("/work/my-links.feat-auth/");
        updated.ports.push(web_port("MAIL_PORT", 20053));

        let registry = parse(&apply(SPEC_EXAMPLE, &updated)).unwrap();

        assert_eq!(registry.instances.len(), 1);
    }

    #[test]
    fn should_keep_the_opted_out_list_and_unknown_tables_when_an_entry_is_added() {
        let before = "version = 1\nopted_out = [\"/work/x\"]\n\n[future]\nkey = 1\n";
        let new = record(
            "/work/b",
            "b",
            (20100, 20149),
            vec![web_port("PORT", 20100)],
        );

        let after = apply(before, &new);

        assert!(after.contains("opted_out = [\"/work/x\"]"));
        assert!(after.contains("[future]\nkey = 1\n"));
    }

    #[test]
    fn should_refuse_a_path_that_is_not_valid_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let (mut document, _) = parse_registry(EMPTY_REGISTRY, Path::new(FILE)).unwrap();
        let mut new = record("/work/b", "b", (20100, 20149), vec![]);
        new.path = PathBuf::from(OsStr::from_bytes(b"/work/\xff"));

        let error = apply_record(&mut document, &new, Path::new(FILE)).unwrap_err();

        assert!(matches!(error, RegistryError::PathNotUtf8 { .. }));
    }

    const OLD_ENTRY: &str = "\n[[instance]]\npath = \"/work/old\"\nname = \"my_app-3f2a\"\nslot = 1\nrange = [20050, 20099]\nports = { PORT = 20050 }\ndetails = { PORT = { kind = \"web\" } }\n";

    fn old_entry(path: &str, name: &str, first: u16) -> String {
        format!(
            "\n[[instance]]\npath = \"{path}\"\nname = \"{name}\"\nslot = 1\nrange = [{first}, {}]\nports = {{ PORT = {first} }}\ndetails = {{ PORT = {{ kind = \"web\" }} }}\n",
            first + 49
        )
    }

    #[test]
    fn should_read_the_stored_host_and_web_port_variable() {
        let text = SPEC_EXAMPLE.replace("slot = 1", "web_port = \"PORT\"\nslot = 1");

        let instance = &parse(&text).unwrap().instances[0];

        assert_eq!(
            (
                instance.host.as_str(),
                instance.web_port_variable.as_deref(),
                instance.is_host_unsaved
            ),
            ("my-links-feat-auth-3f2a.localhost", Some("PORT"), false)
        );
    }

    #[test]
    fn should_read_an_entry_without_host_and_compute_it_from_the_name_when_reading() {
        let text = format!("version = 1\n{OLD_ENTRY}");

        let instance = &parse(&text).unwrap().instances[0];

        assert_eq!(
            (
                instance.host.as_str(),
                instance.web_port_variable.as_deref(),
                instance.is_host_unsaved
            ),
            ("my-app-3f2a.localhost", None, true)
        );
    }

    #[test]
    fn should_compute_a_host_around_the_stored_ones_when_entries_have_none() {
        let stored = entry("/work/a", "my-app-3f2a", (20100, 20149), 20100);
        let text = format!("version = 1\n{OLD_ENTRY}{stored}");

        let registry = parse(&text).unwrap();

        let hosts: Vec<&str> = registry
            .instances
            .iter()
            .map(|instance| instance.host.as_str())
            .collect();
        assert_eq!(hosts, ["my-app-3f2a-1.localhost", "my-app-3f2a.localhost"]);
    }

    #[test]
    fn should_refuse_a_host_that_is_not_a_single_localhost_label() {
        let text = SPEC_EXAMPLE.replace("my-links-feat-auth-3f2a.localhost", "evil.example.com");

        assert_eq!(
            malformed_reason(&text),
            "instance #1 (/work/my-links.feat-auth): host must be one <label>.localhost name of letters, digits and dashes"
        );
    }

    #[test]
    fn should_refuse_two_entries_with_the_same_host() {
        let first = entry("/work/a", "a", (20050, 20099), 20050);
        let second =
            entry("/work/b", "b", (20100, 20149), 20100).replace("b.localhost", "a.localhost");

        assert_eq!(
            malformed_reason(&registry_text(&[first, second])),
            "instance /work/a and instance /work/b: the host a.localhost is used twice"
        );
    }

    #[test]
    fn should_refuse_a_web_port_that_is_not_a_variable_name() {
        let text = SPEC_EXAMPLE.replace("slot = 1", "web_port = \"evil.example.com\"\nslot = 1");

        assert_eq!(
            malformed_reason(&text),
            "instance #1 (/work/my-links.feat-auth): web_port must be a variable name"
        );
    }

    #[test]
    fn should_add_the_host_to_an_old_entry_when_it_is_saved_and_touch_nothing_else() {
        let before = format!("version = 1\n{OLD_ENTRY}");
        let old = parse(&before).unwrap().instances[0].clone();

        let after = apply(&before, &old);

        assert_eq!(after, format!("{before}host = \"my-app-3f2a.localhost\"\n"));
    }

    #[test]
    fn should_never_replace_the_stored_host_when_a_record_carries_another_one() {
        let mut other = parse(SPEC_EXAMPLE).unwrap().instances[0].clone();
        other.host = "changed.localhost".to_owned();

        assert_eq!(apply(SPEC_EXAMPLE, &other), SPEC_EXAMPLE);
    }

    #[test]
    fn should_add_change_and_remove_the_web_port_variable_in_place() {
        let mut record = parse(SPEC_EXAMPLE).unwrap().instances[0].clone();
        record.web_port_variable = Some("PORT".to_owned());
        let added = apply(SPEC_EXAMPLE, &record);
        record.web_port_variable = Some("VITE_HMR_PORT".to_owned());
        let changed = apply(&added, &record);
        record.web_port_variable = None;
        let removed = apply(&changed, &record);

        assert_eq!(
            (
                added.contains("web_port = \"PORT\"\n"),
                changed.contains("web_port = \"VITE_HMR_PORT\"\n"),
                changed.contains("web_port = \"PORT\"\n"),
                removed
            ),
            (true, true, false, SPEC_EXAMPLE.to_owned())
        );
    }

    #[test]
    fn should_round_trip_an_entry_with_a_host_and_a_web_port_variable() {
        let mut record = record(
            "/work/a",
            "a",
            (20050, 20099),
            vec![web_port("PORT", 20050)],
        );
        record.web_port_variable = Some("PORT".to_owned());

        let text = apply(EMPTY_REGISTRY, &record);

        assert_eq!(parse(&text).unwrap().instances, vec![record]);
    }

    #[test]
    fn should_not_make_an_old_registry_malformed_when_no_entry_has_a_host() {
        let text = format!(
            "version = 1\n{}{}",
            old_entry("/work/a", "app-3f2a", 20050),
            old_entry("/work/b", "app_3f2a", 20100)
        );

        let hosts: Vec<String> = parse(&text)
            .unwrap()
            .instances
            .into_iter()
            .map(|instance| instance.host)
            .collect();

        assert_eq!(hosts, ["app-3f2a.localhost", "app-3f2a-1.localhost"]);
    }

    #[test]
    fn should_round_trip_the_project_and_the_worktree_flag_of_an_entry() {
        let mut record = record(
            "/work/a",
            "a",
            (20050, 20099),
            vec![web_port("PORT", 20050)],
        );
        record.origin = Some(origin("my-links", true));

        let text = apply(EMPTY_REGISTRY, &record);

        assert_eq!(parse(&text).unwrap().instances, vec![record]);
    }

    #[test]
    fn should_read_no_origin_when_the_entry_predates_the_keys() {
        assert_eq!(parse(SPEC_EXAMPLE).unwrap().instances[0].origin, None);
    }

    #[test]
    fn should_read_no_origin_and_stay_valid_when_an_origin_key_is_missing_or_unusable() {
        let cases = [
            "project = \"my-links\"\n",
            "worktree = true\n",
            "project = 3\nworktree = true\n",
            "project = \"my-links\"\nworktree = \"yes\"\n",
            "project = \"\"\nworktree = true\n",
        ];

        let origins: Vec<Option<CheckoutOrigin>> = cases
            .iter()
            .map(|keys| {
                let text = SPEC_EXAMPLE.replace("slot = 1\n", &format!("slot = 1\n{keys}"));
                parse(&text).unwrap().instances[0].origin.clone()
            })
            .collect();

        assert_eq!(origins, vec![None; cases.len()]);
    }

    #[test]
    fn should_leave_exactly_one_project_and_one_worktree_key_when_an_unusable_entry_is_saved() {
        let before =
            SPEC_EXAMPLE.replace("slot = 1\n", "slot = 1\nproject = 3\nworktree = \"yes\"\n");
        let mut saved = parse(&before).unwrap().instances[0].clone();
        saved.origin = Some(origin("my-links", false));

        let after = apply(&before, &saved);

        assert_eq!(
            (
                after.matches("project =").count(),
                after.matches("worktree =").count(),
                parse(&after).unwrap().instances[0].origin.clone()
            ),
            (1, 1, Some(origin("my-links", false)))
        );
    }

    #[test]
    fn should_replace_the_origin_in_place_when_it_changes() {
        let mut saved = record(
            "/work/a",
            "a",
            (20050, 20099),
            vec![web_port("PORT", 20050)],
        );
        saved.origin = Some(origin("my-links", false));
        let before = apply(EMPTY_REGISTRY, &saved);
        saved.origin = Some(origin("other", true));

        let after = apply(&before, &saved);

        assert_eq!(
            after,
            before
                .replace("project = \"my-links\"", "project = \"other\"")
                .replace("worktree = false", "worktree = true")
        );
    }

    #[test]
    fn should_leave_the_other_instances_and_comments_byte_for_byte_when_a_save_only_adds_the_origin()
     {
        let first = entry("/work/a", "a", (20050, 20099), 20050);
        let second = entry("/work/b", "b", (20100, 20149), 20100);
        let before = format!("# header\nversion = 1\n{first}# between a and b\n{second}# tail\n");
        let mut updated = parse(&before).unwrap().instances[1].clone();
        updated.origin = Some(origin("my-links", true));

        let after = apply(&before, &updated);

        let expected_second = format!("{second}project = \"my-links\"\nworktree = true\n");
        assert_eq!(
            after,
            format!("# header\nversion = 1\n{first}# between a and b\n{expected_second}# tail\n")
        );
    }
}
