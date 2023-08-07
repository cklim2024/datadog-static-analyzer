# datadog-static-analyzer

datadog-static-analyzer is the static analyzer that powers Datadog [static analysis product](https://docs.datadoghq.com/continuous_integration/static_analysis).

You can use it in your CI/CD pipeline using our integration:
 - [GitHub Action](https://github.com/DataDog/datadog-static-analyzer-github-action)
 - [CircleCI ORB](https://circleci.com/developer/orbs/orb/datadog/datadog-static-analyzer-circleci-orb)

If you use it in your own CI/CD pipeline, you can integrate the tool directly: see the [Datadog documentation for more information](https://docs.datadoghq.com/continuous_integration/static_analysis/?tab=other). 


## Download

Download the latest release for your system and architecture from the [release page](https://github.com/DataDog/datadog-static-analyzer/blob/main/releases/latest).


To get the static analyzer via shell:

```shell
curl -L -O http://www.github.com/DataDog/datadog-static-analyzer/releases/latest/download/datadog-static-analyzer-<target>.zip
```

Example to get the x86_64 binary for Linux:

```shell
curl -L -O http://www.github.com/DataDog/datadog-static-analyzer/releases/latest/download/datadog-static-analyzer-x86_64-unknown-linux-gnu.zip
```

## Usage

```shell
datadog-static-analyzer -i <directory> -o <output-file>
```

For the tool to work, you must have a `<directory>/static-analysis.datadog.yml` file that defines the
configuration of the analyzer. This file will indicate the rules you use for your project.
You can get more information about the configuration on [Datadog documentation](https://docs.datadoghq.com/continuous_integration/static_analysis).

### Mac OS X users

The binary cannot be executed as is. You need to flag the binary as safe to execute using the following command.

```shell
xattr -dr com.apple.quarantine datadog-static-analyzer
```

## Options

 - `-f` or `--format`: format of the output file. `-f sarif` produces a [SARIF-compliant file](https://www.oasis-open.org/committees/tc_home.php?wg_abbrev=sarif)
 - `-r` or `--rules`: provides a file that contains all rules (rules can be put in a file using `datadog-export-rulesets`)


## Configuration

For the tool to work, the following variables must be configured:

 - `DD_APP_KEY`: the application key from Datadog
 - `DD_API_KEY`: the API key from Datadog
 - `DD_SITE`: the Datadog site to use (see list [here](https://docs.datadoghq.com/getting_started/site/))

## Other Tools

### datadog-export-rulesets

Export rulesets from the API into a file

```shell
cargo run --bin datadog-export-rulesets -- -r <ruleset> -o <file-to-export>
```

## Contribute

See file [CONTRIBUTING.md](CONTRIBUTING.md) for more information as well as [DEVELOPMENT.md](DEVELOPMENT.md)
for all details about testing and coding guidelines.

## More information

 - [Datadog Static Analysis](https://docs.datadoghq.com/continuous_integration/static_analysis)
