use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use anyhow::Result;
use base64::Engine;
use git2::{BlameOptions, Repository};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_sarif::sarif::{
    self, ArtifactChangeBuilder, ArtifactLocationBuilder, Fix, FixBuilder, LocationBuilder,
    MessageBuilder, PhysicalLocationBuilder, PropertyBagBuilder, RegionBuilder, Replacement,
    ReportingDescriptor, Result as SarifResult, ResultBuilder, RunBuilder, Sarif, SarifBuilder,
    Tool, ToolBuilder, ToolComponent, ToolComponentBuilder,
};

use crate::constants::{SARIF_PROPERTY_DATADOG_FINGERPRINT, SARIF_PROPERTY_SHA};
use kernel::model::rule::RuleSeverity;
use kernel::model::{
    common::PositionBuilder,
    rule::{Rule, RuleResult},
    violation::{Edit, EditType},
};

use crate::file_utils::get_fingerprint_for_violation;
use crate::model::datadog_api::DiffAwareData;

trait IntoSarif {
    type SarifType;

    fn into_sarif(self) -> Self::SarifType;
}

// Options to use when to generate the SARIF reports.
// if `add_git_info` is true, the git_repo should not be
// optional and will be used to get the SHA of the violations.
#[derive(Clone)]
pub struct SarifGenerationOptions {
    pub add_git_info: bool,
    pub git_repo: Option<Rc<Repository>>,
    pub debug: bool,
    pub config_digest: String,
    pub diff_aware_parameters: Option<DiffAwareData>,
    pub repository_directory: String,
}

impl IntoSarif for &Rule {
    type SarifType = sarif::ReportingDescriptor;

    fn into_sarif(self) -> Self::SarifType {
        let mut builder = sarif::ReportingDescriptorBuilder::default();
        builder.id(&self.name);

        if let Some(d) = self.description_base64.as_ref() {
            let decrypted_description = base64::engine::general_purpose::STANDARD
                .decode(d.as_bytes())
                .unwrap();
            let text_description =
                std::str::from_utf8(&decrypted_description).unwrap_or("invalid full description");
            let text = sarif::MultiformatMessageStringBuilder::default()
                .text(std::str::from_utf8(text_description.as_bytes()).unwrap())
                .build()
                .unwrap();
            builder.full_description(text);
        }

        if let Some(d) = self.short_description_base64.as_ref() {
            let decrypted_description = base64::engine::general_purpose::STANDARD
                .decode(d.as_bytes())
                .unwrap();
            let text_description =
                std::str::from_utf8(&decrypted_description).unwrap_or("invalid short description");
            let text = sarif::MultiformatMessageStringBuilder::default()
                .text(std::str::from_utf8(text_description.as_bytes()).unwrap())
                .build()
                .unwrap();
            builder.short_description(text);
        }

        if let Some(cwe) = self.cwe.as_ref() {
            let props = PropertyBagBuilder::default()
                .tags(vec![format!("CWE:{}", cwe)])
                .build()
                .unwrap();
            builder.properties(props);
        }

        builder.help_uri(self.get_url()).build().unwrap()
    }
}

// TODO: Error handling
impl IntoSarif for &Edit {
    type SarifType = sarif::Replacement;

    fn into_sarif(self) -> Self::SarifType {
        match self.edit_type {
            EditType::Add => sarif::ReplacementBuilder::default()
                .deleted_region(
                    sarif::RegionBuilder::default()
                        .start_line(self.start.line)
                        .start_column(self.start.col)
                        .end_line(self.start.line)
                        .end_column(self.start.col)
                        .build()
                        .unwrap(),
                )
                .inserted_content(
                    sarif::ArtifactContentBuilder::default()
                        .text(self.content.as_ref().unwrap())
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
            EditType::Remove => sarif::ReplacementBuilder::default()
                .deleted_region(
                    sarif::RegionBuilder::default()
                        .start_line(self.start.line)
                        .start_column(self.start.col)
                        .end_line(
                            self.end
                                .clone()
                                .unwrap_or(
                                    PositionBuilder::default().line(0).col(0).build().unwrap(),
                                )
                                .line,
                        )
                        .end_column(
                            self.end
                                .clone()
                                .unwrap_or(
                                    PositionBuilder::default().line(0).col(0).build().unwrap(),
                                )
                                .col,
                        )
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
            EditType::Update => sarif::ReplacementBuilder::default()
                .deleted_region(
                    sarif::RegionBuilder::default()
                        .start_line(self.start.line)
                        .start_column(self.start.col)
                        .end_line(
                            self.end
                                .clone()
                                .unwrap_or(
                                    PositionBuilder::default().line(0).col(0).build().unwrap(),
                                )
                                .line,
                        )
                        .end_column(
                            self.end
                                .clone()
                                .unwrap_or(
                                    PositionBuilder::default().line(0).col(0).build().unwrap(),
                                )
                                .col,
                        )
                        .build()
                        .unwrap(),
                )
                .inserted_content(
                    sarif::ArtifactContentBuilder::default()
                        .text(self.content.as_ref().unwrap())
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
        }
    }
}

// Generate the tool section that reports all the rules being run
fn generate_tool_section(rules: &[Rule], options: &SarifGenerationOptions) -> Result<Tool> {
    let mut tags = vec![];
    tags.push(format!(
        "DATADOG_DIFF_AWARE_CONFIG_DIGEST:{}",
        options.config_digest
    ));

    // if diff-aware is enabled and we got diff-aware data from the backend, we add it in the sarif file
    if let Some(diff_aware) = &options.diff_aware_parameters {
        tags.push("DATADOG_DIFF_AWARE_ENABLED:true".to_string());
        tags.push(format!(
            "DATADOG_DIFF_AWARE_BASE_SHA:{}",
            diff_aware.base_sha
        ));
        diff_aware.files.iter().for_each(|f| {
            tags.push(format!("DATADOG_DIFF_AWARE_FILE:{}", f));
        })
    } else {
        tags.push("DATADOG_DIFF_AWARE_ENABLED:false".to_string());
    }

    let driver: ToolComponent = ToolComponentBuilder::default()
        .name("datadog-static-analyzer")
        .information_uri("https://www.datadoghq.com")
        .rules(
            rules
                .iter()
                .map(|e| e.into_sarif())
                .collect::<Vec<ReportingDescriptor>>(),
        )
        .properties(PropertyBagBuilder::default().tags(tags).build().unwrap())
        .build()?;

    Ok(ToolBuilder::default().driver(driver).build()?)
}

/// Convert our severity enumeration into the corresponding SARIF values.
/// The main discrepancy here is that Notice maps to note.
/// See [this document](https://github.com/oasis-tcs/sarif-spec/blob/main/Documents/CommitteeSpecifications/2.1.0/sarif-schema-2.1.0.json#L1566)
/// for the full SARIF standard.
fn get_level_from_severity(severity: RuleSeverity) -> String {
    match severity {
        RuleSeverity::Notice => "note",
        RuleSeverity::Warning => "warning",
        RuleSeverity::Error => "error",
        _ => "none",
    }
    .to_string()
}

/// Get the latest commit id/sha for a file/line. This is done to know the latest SHA for a line with
/// a violation. Note that this function performs a `git blame` and can take significant time.
/// Take the file/line of the SHA to get and return the SHA if found.
fn get_sha_for_line(
    filename: &str,
    line: usize,
    generation_options: &SarifGenerationOptions,
) -> Option<String> {
    if let Some(git_repo) = generation_options.git_repo.as_ref() {
        if generation_options.debug {
            eprint!(
                "[get_sha_for_line] Getting SHA for file {}, line {}: ",
                filename, line
            );
        }

        let mut blame_options = BlameOptions::default();
        let blame_res = git_repo.blame_file(Path::new(filename), Some(&mut blame_options));

        if let Ok(blame) = blame_res {
            if let Some(hunk) = blame.get_line(line) {
                let commit_id = hunk.final_commit_id().to_string();

                if generation_options.debug {
                    eprintln!("found ({})", commit_id);
                }
                return Some(commit_id);
            } else {
                if generation_options.debug {
                    eprintln!("hunk not found");
                }
                return None;
            }
        }

        if generation_options.debug {
            eprintln!(" cannot get git blame info at {}:{}", filename, line)
        }
        None
    } else {
        None
    }
}

// Encode the file using percent to that filename "My Folder/file.c" is "My%20Folder/file.c"
fn encode_filename(filename: String) -> String {
    const FRAGMENT: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'<')
        .add(b'>')
        .add(b'`')
        .add(b'[')
        .add(b']')
        .add(b'#')
        .add(b'%');

    return utf8_percent_encode(filename.as_str(), FRAGMENT).collect();
}

// Generate the tool section that reports all the rules being run
fn generate_results(
    rules: &[Rule],
    rules_results: &[RuleResult],
    options_orig: SarifGenerationOptions,
) -> Result<Vec<SarifResult>> {
    rules_results
        .iter()
        .flat_map(|rule_result| {
            // if we find the rule for this violation, get the id, level and category
            let mut result_builder = ResultBuilder::default();
            let mut tags = vec![];

            if let Some(rule_index) = rules.iter().position(|r| r.name == rule_result.rule_name) {
                let rule = &rules[rule_index];
                let category = format!("DATADOG_CATEGORY:{}", rule.category).to_uppercase();

                result_builder.rule_index(i64::try_from(rule_index).unwrap());
                result_builder.level(get_level_from_severity(rule.severity));
                tags.push(category);

                // If there is a CWE, add it
                if let Some(cwe) = &rule.cwe {
                    tags.push(format!("CWE:{}", cwe));
                }
            }

            let options = options_orig.clone();
            rule_result.violations.iter().map(move |violation| {
                // if we find the rule for this violation, get the id, level and category
                let location = LocationBuilder::default()
                    .physical_location(
                        PhysicalLocationBuilder::default()
                            .artifact_location(
                                ArtifactLocationBuilder::default()
                                    .uri(encode_filename(rule_result.filename.clone()))
                                    .build()
                                    .unwrap(),
                            )
                            .region(
                                RegionBuilder::default()
                                    .start_line(violation.start.line)
                                    .start_column(violation.start.col)
                                    .end_line(violation.end.line)
                                    .end_column(violation.end.col)
                                    .build()?,
                            )
                            .build()?,
                    )
                    .build()?;

                let fixes: Vec<Fix> = violation
                    .fixes
                    .iter()
                    .map(|fix| {
                        let replacements: Vec<Replacement> =
                            fix.edits.iter().map(IntoSarif::into_sarif).collect();

                        let changes = ArtifactChangeBuilder::default()
                            .artifact_location(
                                ArtifactLocationBuilder::default()
                                    .uri(encode_filename(rule_result.filename.clone()))
                                    .build()?,
                            )
                            .replacements(replacements)
                            .build()?;
                        Ok(FixBuilder::default()
                            .description(
                                MessageBuilder::default()
                                    .text(fix.description.clone())
                                    .build()?,
                            )
                            .artifact_changes(vec![changes])
                            .build()?)
                    })
                    .collect::<Result<Vec<_>>>()?;

                let sha_option = if options.add_git_info {
                    get_sha_for_line(
                        rule_result.filename.as_str(),
                        violation.start.line as usize,
                        &options,
                    )
                } else {
                    None
                };

                let fingerprint_option = get_fingerprint_for_violation(
                    violation,
                    Path::new(options.repository_directory.as_str()),
                    Path::new(rule_result.filename.as_str()),
                    options.debug,
                );

                let partial_fingerprints: BTreeMap<String, String> =
                    match (sha_option, fingerprint_option) {
                        (Some(sha), Some(fp)) => BTreeMap::from([
                            (SARIF_PROPERTY_SHA.to_string(), sha),
                            (SARIF_PROPERTY_DATADOG_FINGERPRINT.to_string(), fp),
                        ]),
                        (None, Some(fp)) => {
                            BTreeMap::from([(SARIF_PROPERTY_DATADOG_FINGERPRINT.to_string(), fp)])
                        }
                        (Some(sha), None) => {
                            BTreeMap::from([(SARIF_PROPERTY_SHA.to_string(), sha)])
                        }
                        _ => BTreeMap::new(),
                    };

                Ok(result_builder
                    .clone()
                    .rule_id(rule_result.rule_name.clone())
                    .locations([location])
                    .fixes(fixes)
                    .message(
                        MessageBuilder::default()
                            .text(violation.message.clone())
                            .build()
                            .unwrap(),
                    )
                    .properties(
                        PropertyBagBuilder::default()
                            .tags(tags.clone())
                            .build()
                            .unwrap(),
                    )
                    .partial_fingerprints(partial_fingerprints)
                    .build()?)
            })
        })
        .collect()
}

// generate a SARIF report for a run.
// the rules parameter is the list of rules used for this run
// the violations parameter is the list of violations for this run.
pub fn generate_sarif_report(
    rules: &[Rule],
    rules_results: &[RuleResult],
    directory: &String,
    add_git_info: bool,
    debug: bool,
    config_digest: String,
    diff_aware_parameters: Option<DiffAwareData>,
) -> Result<Sarif> {
    // if we enable git info, we are then getting the repository object. We put that
    // into an `Arc` object to be able to clone the object.
    let repository: Option<Rc<Repository>> = if add_git_info {
        let repo = Repository::open(directory.as_str());
        if repo.is_err() {
            eprintln!("Invalid Git repository in {}", directory);
            panic!("Please provide a valid Git repository or disable Git integration");
        }
        Some(Rc::new(repo.expect("cannot open repository")))
    } else {
        None
    };

    let options = SarifGenerationOptions {
        add_git_info,
        git_repo: repository,
        debug,
        config_digest,
        diff_aware_parameters,
        repository_directory: directory.clone(),
    };

    let run = RunBuilder::default()
        .tool(generate_tool_section(rules, &options)?)
        .results(generate_results(rules, rules_results, options)?)
        .build()?;

    Ok(SarifBuilder::default()
        .version("2.1.0")
        .runs(vec![run])
        .build()?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use assert_json_diff::assert_json_eq;
    use serde_json::{from_str, Value};
    use valico::json_schema;

    use kernel::model::{
        common::{Language, PositionBuilder},
        rule::{RuleBuilder, RuleCategory, RuleResultBuilder, RuleSeverity, RuleType},
        violation::{EditBuilder, EditType, FixBuilder as RosieFixBuilder, ViolationBuilder},
    };

    use super::*;

    /// Validate JSON data against the SARIF schema
    fn validate_data(v: &Value) -> bool {
        let j_schema = from_str(include_str!("sarif-schema-2.1.0.json")).unwrap();
        let mut scope = json_schema::Scope::new();
        let schema = scope.compile_and_return(j_schema, true).expect("schema");
        schema.validate(v).is_valid()
    }

    // test to check the correct generation of a SARIF report with all the default
    // values. This assumes the happy path and does not stress test the
    // code path.
    #[test]
    fn test_generate_sarif_report_happy_path() {
        let rule = RuleBuilder::default()
            .name("my-rule".to_string())
            .description_base64(Some("YXdlc29tZSBydWxl".to_string()))
            .language(Language::Python)
            .checksum("blabla".to_string())
            .pattern(None)
            .tree_sitter_query_base64(Some("ts-query".to_string()))
            .category(RuleCategory::BestPractices)
            .code_base64("Zm9vYmFyYmF6".to_string())
            .short_description_base64(Some("c2hvcnQgZGVzY3JpcHRpb24=".to_string()))
            .entity_checked(None)
            .rule_type(RuleType::TreeSitterQuery)
            .severity(RuleSeverity::Error)
            .cwe(Some("1234".to_string()))
            .variables(HashMap::new())
            .tests(vec![])
            .build()
            .unwrap();

        let rule_result = RuleResultBuilder::default()
            .rule_name("my-rule".to_string())
            .filename("myfile".to_string())
            .violations(vec![ViolationBuilder::default()
                .start(PositionBuilder::default().line(1).col(2).build().unwrap())
                .end(PositionBuilder::default().line(3).col(4).build().unwrap())
                .message("violation message".to_string())
                .severity(RuleSeverity::Error)
                .category(RuleCategory::BestPractices)
                .fixes(vec![RosieFixBuilder::default()
                    .description("myfix".to_string())
                    .edits(vec![EditBuilder::default()
                        .edit_type(EditType::Add)
                        .start(PositionBuilder::default().line(6).col(6).build().unwrap())
                        .end(Some(
                            PositionBuilder::default().line(6).col(6).build().unwrap(),
                        ))
                        .content(Some("newcontent".to_string()))
                        .build()
                        .unwrap()])
                    .build()
                    .unwrap()])
                .build()
                .unwrap()])
            .output(None)
            .errors(vec![])
            .execution_time_ms(42)
            .execution_error(None)
            .build()
            .expect("building violation");

        let sarif_report = generate_sarif_report(
            &[rule],
            &vec![rule_result],
            &"mydir".to_string(),
            false,
            false,
            "5d7273dec32b80788b4d3eac46c866f0".to_string(),
            None,
        )
        .expect("generate sarif report");

        let sarif_report_to_string = serde_json::to_value(sarif_report).unwrap();
        assert_json_eq!(
            sarif_report_to_string,
            serde_json::json!({"runs":[{"results":[{"fixes":[{"artifactChanges":[{"artifactLocation":{"uri":"myfile"},"replacements":[{"deletedRegion":{"endColumn":6,"endLine":6,"startColumn":6,"startLine":6},"insertedContent":{"text":"newcontent"}}]}],"description":{"text":"myfix"}}],"level":"error","locations":[{"physicalLocation":{"artifactLocation":{"uri":"myfile"},"region":{"endColumn":4,"endLine":3,"startColumn":2,"startLine":1}}}],"message":{"text":"violation message"},"partialFingerprints":{},"properties":{"tags":["DATADOG_CATEGORY:BEST_PRACTICES","CWE:1234"]},"ruleId":"my-rule","ruleIndex":0}],"tool":{"driver":{"informationUri":"https://www.datadoghq.com","name":"datadog-static-analyzer","properties":{"tags":["DATADOG_DIFF_AWARE_CONFIG_DIGEST:5d7273dec32b80788b4d3eac46c866f0","DATADOG_DIFF_AWARE_ENABLED:false"]},"rules":[{"fullDescription":{"text":"awesome rule"},"helpUri":"https://docs.datadoghq.com/static_analysis/rules/my-rule","id":"my-rule","properties":{"tags":["CWE:1234"]},"shortDescription":{"text":"short description"}}]}}}],"version":"2.1.0"})
        );

        // validate the schema
        assert!(validate_data(&sarif_report_to_string));
    }

    // Ensure that diff-aware scanning information are correctly surfaced
    #[test]
    fn test_generate_sarif_diff_aware_scanning() {
        let diff_aware_infos = DiffAwareData {
            base_sha: "d495287772cc8123136b89e8cf5afecbed671823".to_string(),
            files: vec!["path/to/file.py".to_string()],
        };

        let sarif_report = generate_sarif_report(
            &[],
            &vec![],
            &"mydir".to_string(),
            false,
            false,
            "5d7273dec32b80788b4d3eac46c866f0".to_string(),
            Some(diff_aware_infos),
        )
        .expect("generate sarif report");

        let sarif_report_to_string = serde_json::to_value(sarif_report).unwrap();
        assert_json_eq!(
            sarif_report_to_string,
            serde_json::json!({"runs":[{"results":[],"tool":{"driver":{"informationUri":"https://www.datadoghq.com","name":"datadog-static-analyzer","properties":{"tags":["DATADOG_DIFF_AWARE_CONFIG_DIGEST:5d7273dec32b80788b4d3eac46c866f0","DATADOG_DIFF_AWARE_ENABLED:true","DATADOG_DIFF_AWARE_BASE_SHA:d495287772cc8123136b89e8cf5afecbed671823","DATADOG_DIFF_AWARE_FILE:path/to/file.py"]},"rules":[]}}}],"version":"2.1.0"})
        );

        // validate the schema
        assert!(validate_data(&sarif_report_to_string));
    }

    #[test]
    fn test_generate_with_escape_characters() {
        let rule = RuleBuilder::default()
            .name("my-rule".to_string())
            .description_base64(Some("YXdlc29tZSBydWxl".to_string()))
            .language(Language::Python)
            .checksum("blabla".to_string())
            .pattern(None)
            .tree_sitter_query_base64(Some("ts-query".to_string()))
            .category(RuleCategory::BestPractices)
            .code_base64("Zm9vYmFyYmF6".to_string())
            .short_description_base64(Some("c2hvcnQgZGVzY3JpcHRpb24=".to_string()))
            .entity_checked(None)
            .rule_type(RuleType::TreeSitterQuery)
            .severity(RuleSeverity::Error)
            .cwe(Some("1234".to_string()))
            .variables(HashMap::new())
            .tests(vec![])
            .build()
            .unwrap();

        let rule_result = RuleResultBuilder::default()
            .rule_name("my-rule".to_string())
            .filename("my file/in my directory".to_string())
            .violations(vec![ViolationBuilder::default()
                .start(PositionBuilder::default().line(1).col(2).build().unwrap())
                .end(PositionBuilder::default().line(3).col(4).build().unwrap())
                .message("violation message".to_string())
                .severity(RuleSeverity::Error)
                .category(RuleCategory::BestPractices)
                .fixes(vec![RosieFixBuilder::default()
                    .description("myfix".to_string())
                    .edits(vec![EditBuilder::default()
                        .edit_type(EditType::Add)
                        .start(PositionBuilder::default().line(6).col(6).build().unwrap())
                        .end(Some(
                            PositionBuilder::default().line(6).col(6).build().unwrap(),
                        ))
                        .content(Some("newcontent".to_string()))
                        .build()
                        .unwrap()])
                    .build()
                    .unwrap()])
                .build()
                .unwrap()])
            .output(None)
            .errors(vec![])
            .execution_time_ms(42)
            .execution_error(None)
            .build()
            .expect("building violation");

        let sarif_report = generate_sarif_report(
            &[rule],
            &vec![rule_result],
            &"mydir".to_string(),
            false,
            false,
            "5d7273dec32b80788b4d3eac46c866f0".to_string(),
            None,
        )
        .expect("generate sarif report");

        let sarif_report_to_string = serde_json::to_value(sarif_report).unwrap();
        assert_json_eq!(
            sarif_report_to_string,
            serde_json::json!({"runs":[{"results":[{"fixes":[{"artifactChanges":[{"artifactLocation":{"uri":"my%20file/in%20my%20directory"},"replacements":[{"deletedRegion":{"endColumn":6,"endLine":6,"startColumn":6,"startLine":6},"insertedContent":{"text":"newcontent"}}]}],"description":{"text":"myfix"}}],"level":"error","locations":[{"physicalLocation":{"artifactLocation":{"uri":"my%20file/in%20my%20directory"},"region":{"endColumn":4,"endLine":3,"startColumn":2,"startLine":1}}}],"message":{"text":"violation message"},"partialFingerprints":{},"properties":{"tags":["DATADOG_CATEGORY:BEST_PRACTICES","CWE:1234"]},"ruleId":"my-rule","ruleIndex":0}],"tool":{"driver":{"informationUri":"https://www.datadoghq.com","name":"datadog-static-analyzer","properties":{"tags":["DATADOG_DIFF_AWARE_CONFIG_DIGEST:5d7273dec32b80788b4d3eac46c866f0","DATADOG_DIFF_AWARE_ENABLED:false"]},"rules":[{"fullDescription":{"text":"awesome rule"},"helpUri":"https://docs.datadoghq.com/static_analysis/rules/my-rule","id":"my-rule","properties":{"tags":["CWE:1234"]},"shortDescription":{"text":"short description"}}]}}}],"version":"2.1.0"})
        );

        // validate the schema
        assert!(validate_data(&sarif_report_to_string));
    }

    // in this test, the rule in the violation cannot be found in the list
    // of rules and the rule index in the sarif report must be empty
    #[test]
    fn test_generate_rule_not_found_rule() {
        let rule = RuleBuilder::default()
            .name("my-rule1".to_string())
            .description_base64(Some("YXdlc29tZSBydWxl".to_string()))
            .language(Language::Python)
            .checksum("blabla".to_string())
            .pattern(None)
            .tree_sitter_query_base64(Some("ts-query".to_string()))
            .category(RuleCategory::BestPractices)
            .code_base64("Zm9vYmFyYmF6".to_string())
            .short_description_base64(Some("c2hvcnQgZGVzY3JpcHRpb24=".to_string()))
            .entity_checked(None)
            .rule_type(RuleType::TreeSitterQuery)
            .severity(RuleSeverity::Error)
            .variables(HashMap::new())
            .cwe(None)
            .tests(vec![])
            .build()
            .unwrap();

        let rule_result = RuleResultBuilder::default()
            .rule_name("my-rule2".to_string())
            .filename("myfile".to_string())
            .violations(vec![ViolationBuilder::default()
                .start(PositionBuilder::default().line(1).col(2).build().unwrap())
                .end(PositionBuilder::default().line(3).col(4).build().unwrap())
                .message("violation message".to_string())
                .severity(RuleSeverity::Error)
                .category(RuleCategory::BestPractices)
                .fixes(vec![RosieFixBuilder::default()
                    .description("myfix".to_string())
                    .edits(vec![EditBuilder::default()
                        .edit_type(EditType::Add)
                        .start(PositionBuilder::default().line(6).col(6).build().unwrap())
                        .end(Some(
                            PositionBuilder::default().line(6).col(6).build().unwrap(),
                        ))
                        .content(Some("newcontent".to_string()))
                        .build()
                        .unwrap()])
                    .build()
                    .unwrap()])
                .build()
                .unwrap()])
            .output(None)
            .errors(vec![])
            .execution_time_ms(42)
            .execution_error(None)
            .build()
            .expect("building violation");

        let sarif_report = generate_sarif_report(
            &[rule],
            &vec![rule_result],
            &"mydir".to_string(),
            false,
            false,
            "5d7273dec32b80788b4d3eac46c866f0".to_string(),
            None,
        )
        .expect("generate sarif report");
        assert!(sarif_report
            .runs
            .get(0)
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .get(0)
            .unwrap()
            .rule_index
            .is_none());
        // validate the schema
        assert!(validate_data(&serde_json::to_value(sarif_report).unwrap()));
    }
}
