param(
    [Parameter(Mandatory = $true)]
    [string]$GraphCsv,

    [string[]]$UrlLists = @()
)

$ErrorActionPreference = 'Stop'

function Get-Percentile {
    param(
        [int[]]$Values,
        [ValidateRange(0.0, 1.0)]
        [double]$Percentile
    )

    if ($Values.Count -eq 0) {
        return 0
    }
    $ordered = @($Values | Sort-Object)
    $index = [Math]::Min(
        $ordered.Count - 1,
        [Math]::Max(0, [Math]::Ceiling($Percentile * $ordered.Count) - 1)
    )
    return $ordered[$index]
}

function Test-WikipediaArticleUrl {
    param([string]$Value)

    $uri = $null
    return [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref]$uri) -and
        $uri.Scheme -eq 'https' -and
        $uri.Host -eq 'de.wikipedia.org' -and
        $uri.AbsolutePath.StartsWith('/wiki/', [StringComparison]::Ordinal)
}

$resolvedGraph = [System.IO.Path]::GetFullPath($GraphCsv)
$rows = @(Import-Csv -LiteralPath $resolvedGraph)
$requiredColumns = @('parent_title', 'parent_url', 'subtopic_title', 'subtopic_url')
$actualColumns = if ($rows.Count -gt 0) {
    @($rows[0].PSObject.Properties.Name)
}
else {
    @()
}
$missingColumns = @($requiredColumns | Where-Object { $_ -notin $actualColumns })
if ($missingColumns.Count -gt 0) {
    throw "Missing required CSV columns: $($missingColumns -join ', ')"
}

$completeRows = @($rows | Where-Object {
    -not [string]::IsNullOrWhiteSpace($_.parent_title) -and
    -not [string]::IsNullOrWhiteSpace($_.parent_url) -and
    -not [string]::IsNullOrWhiteSpace($_.subtopic_title) -and
    -not [string]::IsNullOrWhiteSpace($_.subtopic_url)
})
$invalidUrlRows = @($completeRows | Where-Object {
    -not (Test-WikipediaArticleUrl $_.parent_url) -or
    -not (Test-WikipediaArticleUrl $_.subtopic_url)
})
$selfLoops = @($completeRows | Where-Object { $_.parent_url -eq $_.subtopic_url })
$pairGroups = @($completeRows | Group-Object parent_url, subtopic_url)
$duplicatePairGroups = @($pairGroups | Where-Object Count -gt 1)
$parentGroups = @($completeRows | Group-Object parent_url)
$childGroups = @($completeRows | Group-Object subtopic_url)
$parentDegrees = @($parentGroups | ForEach-Object { $_.Count })
$childDegrees = @($childGroups | ForEach-Object { $_.Count })
$parentCount = $parentGroups.Count
$universalThreshold = [Math]::Max(2, [Math]::Ceiling($parentCount * 0.8))
$universalHubCandidates = @($childGroups | Where-Object Count -ge $universalThreshold |
    Sort-Object Count -Descending | ForEach-Object {
        [ordered]@{
            title = $_.Group[0].subtopic_title
            url = $_.Name
            parent_count = $_.Count
            parent_share = [Math]::Round($_.Count / [Math]::Max(1, $parentCount), 4)
        }
    })

$knownNamespacePattern = '^(Benutzer|Wikipedia|Diskussion|Portal|Kategorie|Datei|Hilfe|Vorlage|Modul|MediaWiki|Spezial):'
$namespaceCounts = @($childGroups | ForEach-Object {
    $title = $_.Group[0].subtopic_title
    if ($title -match $knownNamespacePattern) {
        $matches[1]
    }
    else {
        'Main/article title'
    }
} | Group-Object | Sort-Object Count -Descending | ForEach-Object {
    [ordered]@{ namespace = $_.Name; unique_urls = $_.Count }
})

$graphChildSet = [System.Collections.Generic.HashSet[string]]::new(
    [StringComparer]::OrdinalIgnoreCase
)
$graphParentSet = [System.Collections.Generic.HashSet[string]]::new(
    [StringComparer]::OrdinalIgnoreCase
)
foreach ($row in $completeRows) {
    [void]$graphChildSet.Add($row.subtopic_url.Trim())
    [void]$graphParentSet.Add($row.parent_url.Trim())
}

$listProfiles = @()
foreach ($urlList in $UrlLists) {
    $resolvedList = [System.IO.Path]::GetFullPath($urlList)
    $rawUrls = @(Get-Content -LiteralPath $resolvedList | ForEach-Object {
        $_.Trim()
    } | Where-Object { $_ })
    $exactSet = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::Ordinal
    )
    $canonicalSet = [System.Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase
    )
    $validCount = 0
    $childCoverage = 0
    $parentCoverage = 0
    foreach ($url in $rawUrls) {
        [void]$exactSet.Add($url)
        [void]$canonicalSet.Add($url)
        if (Test-WikipediaArticleUrl $url) {
            $validCount++
        }
    }
    foreach ($url in $canonicalSet) {
        if ($graphChildSet.Contains($url)) {
            $childCoverage++
        }
        if ($graphParentSet.Contains($url)) {
            $parentCoverage++
        }
    }
    $listProfiles += [ordered]@{
        path = $resolvedList
        rows = $rawUrls.Count
        exact_unique_urls = $exactSet.Count
        exact_duplicate_rows = $rawUrls.Count - $exactSet.Count
        casefolded_unique_urls = $canonicalSet.Count
        case_variant_or_duplicate_rows = $rawUrls.Count - $canonicalSet.Count
        valid_wikipedia_article_rows = $validCount
        graph_child_coverage = "$childCoverage/$($graphChildSet.Count)"
        graph_parent_coverage = "$parentCoverage/$($graphParentSet.Count)"
    }
}

$summary = [ordered]@{
    schema = 'voxel-native/research-link-audit/v1'
    graph_path = $resolvedGraph
    grain = 'one directed parent_url -> subtopic_url relation per CSV row'
    rows = $rows.Count
    complete_rows = $completeRows.Count
    incomplete_rows = $rows.Count - $completeRows.Count
    invalid_wikipedia_url_rows = $invalidUrlRows.Count
    self_loops = $selfLoops.Count
    unique_parent_urls = $parentGroups.Count
    unique_subtopic_urls = $childGroups.Count
    unique_directed_pairs = $pairGroups.Count
    duplicate_directed_pair_rows = $completeRows.Count - $pairGroups.Count
    duplicate_pair_groups = @($duplicatePairGroups | ForEach-Object {
        [ordered]@{
            count = $_.Count
            parent_title = $_.Group[0].parent_title
            subtopic_title = $_.Group[0].subtopic_title
        }
    })
    parent_degree = [ordered]@{
        min = Get-Percentile $parentDegrees 0.0
        median = Get-Percentile $parentDegrees 0.5
        p95 = Get-Percentile $parentDegrees 0.95
        max = Get-Percentile $parentDegrees 1.0
    }
    subtopic_degree = [ordered]@{
        min = Get-Percentile $childDegrees 0.0
        median = Get-Percentile $childDegrees 0.5
        p95 = Get-Percentile $childDegrees 0.95
        max = Get-Percentile $childDegrees 1.0
    }
    universal_hub_threshold = "$universalThreshold/$parentCount parents"
    universal_hub_candidates = $universalHubCandidates
    namespace_profile = $namespaceCounts
    url_lists = $listProfiles
}

$summary | ConvertTo-Json -Depth 8
