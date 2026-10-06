#!/usr/bin/env Rscript

suppressPackageStartupMessages({
  library(data.table)
  library(dplyr)
  library(jsonlite)
  library(openxlsx)
  library(yaml)
  library(zoo)
})

arg_value <- function(args, name, default) {
  prefix <- paste0(name, "=")
  match <- args[startsWith(args, prefix)]
  if (length(match) == 0) {
    return(default)
  }
  sub(prefix, "", match[[1]], fixed = TRUE)
}

sha256_file <- function(path) {
  output <- system2("shasum", c("-a", "256", path), stdout = TRUE)
  strsplit(output[[1]], " ")[[1]][[1]]
}

git_output <- function(repo, args) {
  system2("git", c("-C", repo, args), stdout = TRUE)
}

round_numeric_columns <- function(frame) {
  as.data.frame(lapply(frame, function(column) {
    if (is.numeric(column)) {
      column <- round(column, 6)
      column[!is.finite(column)] <- NA_real_
      return(column)
    }
    column
  }))
}

args <- commandArgs(trailingOnly = TRUE)
mode <- arg_value(args, "--mode", "expanded")
if (!(mode %in% c("baseline", "expanded"))) {
  stop("--mode must be either baseline or expanded")
}

repo <- arg_value(
  args,
  "--repo",
  Sys.getenv(
    "TREASURY_INCOME_EXPLORER_PATH",
    unset = "/Users/maxghenis/_axiom-worktrees/nz-treasury-income-explorer"
  )
)
parameter_file <- arg_value(
  args,
  "--parameter-file",
  file.path(repo, "inst/parameters/TY27_BEFU25.yaml")
)
output_path <- arg_value(
  args,
  "--output",
  if (mode == "baseline") {
    "data/oracles/treasury-emtr-snapshot.json"
  } else {
    "ops/nz-lane/emtr_reproduction/treasury-emtr-snapshot-expanded.json"
  }
)

source(file.path(repo, "R", "params_template.R"))
source(file.path(repo, "R", "util.R"))
source(file.path(repo, "R", "emtr.R"))

parameters <- parameters_from_file(parameter_file)

sample_weekly_gross_wage <- c(0, 160, 250, 370, 555, 740, 1000, 1500)
output_columns <- c(
  "gross_wage1",
  "hours1",
  "gross_wage1_annual",
  "gross_wage2",
  "wage1_tax",
  "wage1_ACC_levy",
  "net_wage1",
  "net_wage",
  "net_benefit",
  "FTC_abated",
  "IWTC_abated",
  "MFTC",
  "IETC_abated",
  "WinterEnergy",
  "BestStart_Total",
  "AS_Amount",
  "WFF_abated",
  "Net_Income",
  "Net_Income_annual",
  "EMTR",
  "RR",
  "PTR"
)

baseline_scenarios <- list(
  list(
    id = "single_parent_three_children_area1_rent",
    description = "Single parent, children aged 0, 1, and 10, Area 1 rent.",
    Partnered = FALSE,
    wage1_hourly = 18.5,
    Children_ages = c(0, 1, 10),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 600,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 1L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "couple_two_children_area2_mortgage",
    description = "Couple, children aged 2 and 15, partner not working, Area 2 mortgage.",
    Partnered = TRUE,
    wage1_hourly = 18.5,
    Children_ages = c(2, 15),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 800,
    AS_Accommodation_Rent = FALSE,
    AS_Area = 2L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "couple_one_child_partner_10h_area3_rent",
    description = "Couple, child aged 9, partner working 10 hours, Area 3 rent.",
    Partnered = TRUE,
    wage1_hourly = 18.5,
    Children_ages = c(9),
    gross_wage2 = 185,
    hours2 = 10,
    AS_Accommodation_Costs = 600,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 3L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "single_no_children_area2_no_housing_costs",
    description = "Single adult, no children, no qualifying housing costs.",
    Partnered = FALSE,
    wage1_hourly = 18.5,
    Children_ages = c(),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = numeric(0)
  )
)

expanded_only_scenarios <- list(
  list(
    id = "lone_parent_two_teens_jss",
    description = "Lone parent, children aged 14 and 16, exercising the separate JSS branch.",
    Partnered = FALSE,
    wage1_hourly = 18.5,
    Children_ages = c(14, 16),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "couple_two_best_start_children_binding",
    description = "Couple with children aged 1 and 2 and a full-time working partner, crossing Best Start abatement kinks.",
    Partnered = TRUE,
    wage1_hourly = 18.5,
    Children_ages = c(1, 2),
    gross_wage2 = 740,
    hours2 = 40,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = c(775, 776, 1125, 1126, 1476, 1477)
  ),
  list(
    id = "couple_two_children_dual_full_time",
    description = "Couple with children aged 6 and 15 and two full-time earners.",
    Partnered = TRUE,
    wage1_hourly = 18.5,
    Children_ages = c(6, 15),
    gross_wage2 = 740,
    hours2 = 40,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = c(121, 122)
  ),
  list(
    id = "single_childless_ietc_focused",
    description = "Childless single adult with focused points around the IETC eligibility and abatement kinks.",
    Partnered = FALSE,
    wage1_hourly = 25,
    Children_ages = c(),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = c(
      688, 689, 692, 693, 1265, 1266, 1342, 1343
    )
  ),
  list(
    id = "lone_parent_two_children_area4_high_rent_cap",
    description = "Lone parent with two children, Area 4 rent high enough to bind the Accommodation Supplement maximum.",
    Partnered = FALSE,
    wage1_hourly = 18.5,
    Children_ages = c(6, 15),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 600,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 4L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "couple_childless_boarder_proxy",
    description = "Childless couple boarder proxy using Treasury's rent input at 62 percent of actual board.",
    Partnered = TRUE,
    wage1_hourly = 18.5,
    Children_ages = c(),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 248,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 2L,
    additional_weekly_gross_wage = numeric(0)
  ),
  list(
    id = "large_family_four_children_age_bands",
    description = "Lone parent with four children aged 3, 6, 13, and 16 spanning child age bands.",
    Partnered = FALSE,
    wage1_hourly = 18.5,
    Children_ages = c(3, 6, 13, 16),
    gross_wage2 = 0,
    hours2 = 0,
    AS_Accommodation_Costs = 0,
    AS_Accommodation_Rent = TRUE,
    AS_Area = 1L,
    additional_weekly_gross_wage = numeric(0)
  )
)

scenarios <- if (mode == "baseline") {
  baseline_scenarios
} else {
  c(baseline_scenarios, expanded_only_scenarios)
}

emtr_component_columns <- paste0("EMTR_", net_income_components)

scenario_results <- lapply(scenarios, function(spec) {
  selected_wages <- sort(unique(c(
    sample_weekly_gross_wage,
    spec$additional_weekly_gross_wage
  )))

  model_output <- emtr(
    Parameters = parameters,
    Partnered = spec$Partnered,
    wage1_hourly = spec$wage1_hourly,
    Children_ages = spec$Children_ages,
    gross_wage2 = spec$gross_wage2,
    hours2 = spec$hours2,
    AS_Accommodation_Costs = spec$AS_Accommodation_Costs,
    AS_Accommodation_Rent = spec$AS_Accommodation_Rent,
    AS_Area = spec$AS_Area,
    max_wage = max(selected_wages),
    steps_per_dollar = 1L
  )

  selected <- model_output[gross_wage1 %in% selected_wages, ..output_columns]
  selected <- selected[order(gross_wage1)]

  selected_emtr_components <- model_output[
    gross_wage1 %in% selected_wages,
    c("gross_wage1", emtr_component_columns),
    with = FALSE
  ]
  selected_emtr_components <- selected_emtr_components[order(gross_wage1)]

  list(
    snapshot = list(
      id = spec$id,
      description = spec$description,
      inputs = spec[setdiff(
        names(spec),
        c("id", "description", "additional_weekly_gross_wage")
      )],
      sampled_outputs = round_numeric_columns(selected)
    ),
    emtr_components = round_numeric_columns(selected_emtr_components)
  )
})

scenario_outputs <- lapply(scenario_results, function(result) result$snapshot)
scenario_emtr_components <- setNames(
  lapply(scenario_results, function(result) result$emtr_components),
  vapply(scenarios, function(spec) spec$id, character(1))
)

rulespec_profile <- function(
    partnered,
    wage1_hourly,
    children_ages,
    partner_weekly_wage,
    partner_hours,
    accommodation_cost,
    accommodation_type,
    accommodation_area,
    boarder = FALSE) {
  list(
    partnered = partnered,
    wage1_hourly = wage1_hourly,
    children_ages = children_ages,
    partner_weekly_wage = partner_weekly_wage,
    partner_hours = partner_hours,
    accommodation_cost = accommodation_cost,
    accommodation_type = accommodation_type,
    accommodation_area = accommodation_area,
    boarder = boarder
  )
}

provenance_entry <- function(
    coverage_tags,
    additional_wages,
    treasury_source_lines,
    profile,
    convention_note = NULL) {
  entry <- list(
    coverage_tags = coverage_tags,
    displayed_weekly_gross_wage = sample_weekly_gross_wage,
    additional_weekly_gross_wage = additional_wages,
    treasury_source_lines = treasury_source_lines,
    rulespec_profile = profile
  )
  if (!is.null(convention_note)) {
    entry$convention_note <- convention_note
  }
  entry
}

scenario_provenance <- list(
  single_parent_three_children_area1_rent = provenance_entry(
    c("continuity_baseline", "lone_parent", "best_start", "area_1_rent"),
    numeric(0),
    c(
      "R/emtr.R:300-350 lone-parent benefit and Accommodation Supplement branches",
      "R/emtr.R:493-575 FTC, IWTC, MFTC, and Best Start calculations"
    ),
    rulespec_profile(
      FALSE, 18.5, c(0, 1, 10), 0, 0, 600, "rent", 1L
    )
  ),
  couple_two_children_area2_mortgage = provenance_entry(
    c("continuity_baseline", "couple", "area_2_mortgage"),
    numeric(0),
    c(
      "R/emtr.R:285-350 partnered benefit and Accommodation Supplement branches",
      "R/emtr.R:493-575 FTC, IWTC, MFTC, and Best Start calculations"
    ),
    rulespec_profile(
      TRUE, 18.5, c(2, 15), 0, 0, 800, "mortgage", 2L
    )
  ),
  couple_one_child_partner_10h_area3_rent = provenance_entry(
    c("continuity_baseline", "partner_wage", "area_3_rent"),
    numeric(0),
    c(
      "R/emtr.R:357-490 partner wage, tax, and ACC calculations",
      "R/emtr.R:523-547 joint family-income WFF abatement"
    ),
    rulespec_profile(
      TRUE, 18.5, c(9), 185, 10, 600, "rent", 3L
    )
  ),
  single_no_children_area2_no_housing_costs = provenance_entry(
    c("continuity_baseline", "childless_single", "no_housing_costs"),
    numeric(0),
    c(
      "R/emtr.R:300-307 childless-single JSS branch",
      "R/emtr.R:578-599 Independent Earner Tax Credit calculation"
    ),
    rulespec_profile(
      FALSE, 18.5, c(), 0, 0, 0, "rent", 2L
    )
  ),
  lone_parent_two_teens_jss = provenance_entry(
    c("lone_parent", "youngest_child_14_plus", "jss_sole_parent_branch"),
    numeric(0),
    c(
      "R/emtr.R:300-322 lone-parent JSS rate with the source-defined SPS abatement scale",
      "R/emtr.R:324-350 lone-parent Accommodation Supplement cutout and area maximum"
    ),
    rulespec_profile(
      FALSE, 18.5, c(14, 16), 0, 0, 0, "rent", 2L
    )
  ),
  couple_two_best_start_children_binding = provenance_entry(
    c("best_start", "aggregate_child_abatement", "partner_wage", "dense_kinks"),
    c(775, 776, 1125, 1126, 1476, 1477),
    c(
      "R/emtr.R:552-575 aggregate Best Start amount and family-income abatement",
      "R/emtr.R:523-547 joint family-income WFF abatement"
    ),
    rulespec_profile(
      TRUE, 18.5, c(1, 2), 740, 40, 0, "rent", 2L
    )
  ),
  couple_two_children_dual_full_time = provenance_entry(
    c("dual_full_time", "partner_tax", "partner_acc", "joint_wff_abatement"),
    c(121, 122),
    c(
      "R/emtr.R:357-490 partner wage, benefit gross-up, tax, and ACC calculations",
      "R/emtr.R:498-547 joint MFTC eligibility and WFF abatement"
    ),
    rulespec_profile(
      TRUE, 18.5, c(6, 15), 740, 40, 0, "rent", 2L
    )
  ),
  single_childless_ietc_focused = provenance_entry(
    c("ietc", "childless_single", "no_main_benefit", "dense_kinks"),
    c(688, 689, 692, 693, 1265, 1266, 1342, 1343),
    c(
      "R/emtr.R:251-269 weekly IETC scale, rate, and minimum-income conversion",
      "R/emtr.R:578-599 IETC eligibility, amount, and abatement"
    ),
    rulespec_profile(
      FALSE, 25, c(), 0, 0, 0, "rent", 2L
    )
  ),
  lone_parent_two_children_area4_high_rent_cap = provenance_entry(
    c("area_4_rent", "high_rent", "accommodation_maximum", "lone_parent"),
    numeric(0),
    c(
      "R/emtr.R:197-226 Accommodation Supplement maxima by area and family type",
      "R/emtr.R:324-350 and 635-640 entry threshold, area maximum, and capped payment"
    ),
    rulespec_profile(
      FALSE, 18.5, c(6, 15), 0, 0, 600, "rent", 4L
    )
  ),
  couple_childless_boarder_proxy = provenance_entry(
    c("boarder_proxy", "childless_couple", "accommodation_supplement"),
    numeric(0),
    c(
      "R/emtr.R:164-183 exposes accommodation costs, rent/mortgage, and area but no boarder input",
      "R/emtr.R:635-640 applies the supplied accommodation cost directly in the rent formula"
    ),
    rulespec_profile(
      TRUE, 18.5, c(), 0, 0, 400, "board", 2L, boarder = TRUE
    ),
    paste0(
      "Actual board/lodgings are NZD 400 per week and the RuleSpec profile sets ",
      "boarder=true. Treasury has no boarder input, so it receives the rent-equivalent ",
      "NZD 248 (=62% of NZD 400). This is a convention proxy, not native boarder validation."
    )
  ),
  large_family_four_children_age_bands = provenance_entry(
    c(
      "large_family", "four_children", "ftc_subsequent_child",
      "iwtc_subsequent_child", "winter_energy_dependent_rate"
    ),
    numeric(0),
    c(
      "R/emtr.R:493-495 FTC eldest-plus-subsequent-child composition",
      "R/emtr.R:446-448 IWTC subsequent-child scaling and 608-626 dependent Winter Energy rate"
    ),
    rulespec_profile(
      FALSE, 18.5, c(3, 6, 13, 16), 0, 0, 0, "rent", 1L
    )
  )
)

snapshot <- list(
  generated_at = if (mode == "baseline") "2026-06-17" else "2026-07-29",
  oracle = list(
    id = "treasury-income-explorer",
    name = "NZ Treasury IncomeExplorer",
    url = "https://github.com/Treasury-Analytics-and-Insights/IncomeExplorer",
    local_path = normalizePath(repo),
    commit = git_output(repo, c("rev-parse", "HEAD"))[[1]],
    commit_date = git_output(repo, c("log", "-1", "--format=%cI"))[[1]],
    parameter_file = sub(
      paste0(normalizePath(repo), "/"),
      "",
      normalizePath(parameter_file)
    ),
    parameter_file_sha256 = sha256_file(parameter_file),
    parameter_vintage = "TY27_BEFU25",
    model_year = parameters$modelyear
  ),
  generator = list(
    adapter = if (mode == "baseline") {
      "treasury-income-explorer-emtr-snapshot"
    } else {
      "treasury-income-explorer-emtr-snapshot-expanded"
    },
    treasury_function = "R/emtr.R#emtr",
    sampled_weekly_gross_wage = sample_weekly_gross_wage,
    output_columns = output_columns,
    note = "Treasury outputs are weekly unless the column name says annual."
  )
)

if (mode == "expanded") {
  for (scenario_id in names(scenario_provenance)) {
    scenario_provenance[[scenario_id]]$treasury_emtr_components <-
      scenario_emtr_components[[scenario_id]]
  }
  snapshot$scenario_provenance <- scenario_provenance
}
snapshot$scenarios <- scenario_outputs

dir.create(dirname(output_path), recursive = TRUE, showWarnings = FALSE)
write_json(
  snapshot,
  output_path,
  pretty = TRUE,
  auto_unbox = TRUE,
  digits = NA,
  na = "null"
)
cat(output_path, "\n")
