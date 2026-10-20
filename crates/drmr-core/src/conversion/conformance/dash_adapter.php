<?php
// Invoke the installed DASH-IF MPD checks without its segment downloader or server.
// The parent owns process-tree cancellation and bounds this protocol's stdout/stderr.
declare(strict_types=1);

ini_set('display_errors', '0');
ini_set('log_errors', '0');
error_reporting(0);

$report = [
    'kind' => 'drmr-dash-manifest-validation',
    'schema_version' => 1,
    'scope' => 'manifest-only',
    'engine' => 'dash-if',
    'engine_revision' => null,
    'manifest_sha256' => '',
    'checks' => [
        'xsd' => ['completed' => false, 'passed' => false, 'issue_count' => 0],
        'schematron' => ['completed' => false, 'passed' => false, 'issue_count' => 0],
    ],
    'passed' => false,
];
$workspace = null;
$identity = null;
$exit = 2;

function local_revision(string $root): ?string
{
    $head = @file_get_contents($root . '/.git/HEAD', false, null, 0, 256);
    if (!is_string($head)) {
        return null;
    }
    $head = trim($head);
    if (preg_match('/\A[0-9a-f]{40}\z/D', $head) === 1) {
        return $head;
    }
    if (preg_match('/\Aref: (refs\/[A-Za-z0-9_\/-]+)\z/D', $head, $match) !== 1
        || str_contains($match[1], '..')) {
        return null;
    }
    $revision = @file_get_contents($root . '/.git/' . $match[1], false, null, 0, 128);
    if (is_string($revision) && preg_match('/\A[0-9a-f]{40}\s*\z/D', $revision) === 1) {
        return trim($revision);
    }
    $packed = @file_get_contents($root . '/.git/packed-refs', false, null, 0, 1048576);
    if (is_string($packed)) {
        foreach (explode("\n", $packed) as $line) {
            if (preg_match('/\A([0-9a-f]{40}) ' . preg_quote($match[1], '/') . '\z/D', $line, $entry) === 1) {
                return $entry[1];
            }
        }
    }
    return null;
}

function remove_owned_workspace(string $path, array $expected): void
{
    $actual = @lstat($path);
    if (!$actual || $actual['dev'] !== $expected['dev'] || $actual['ino'] !== $expected['ino']
        || is_link($path) || !is_dir($path)) {
        throw new RuntimeException('workspace-identity');
    }
    $entries = new RecursiveIteratorIterator(
        new RecursiveDirectoryIterator($path, FilesystemIterator::SKIP_DOTS),
        RecursiveIteratorIterator::CHILD_FIRST
    );
    foreach ($entries as $entry) {
        $name = $entry->getPathname();
        if ($entry->isLink() || !$entry->isDir()) {
            if (!unlink($name)) {
                throw new RuntimeException('workspace-cleanup');
            }
        } elseif (!rmdir($name)) {
            throw new RuntimeException('workspace-cleanup');
        }
    }
    if (!rmdir($path)) {
        throw new RuntimeException('workspace-cleanup');
    }
}

try {
    if (PHP_VERSION_ID < 80400 || count($argv) !== 2) {
        throw new RuntimeException('unsupported-runtime');
    }
    $root = getenv('DRMR_DASH_IF_ROOT');
    if (!is_string($root) || $root === '' || !is_dir($root)) {
        throw new RuntimeException('engine-not-configured');
    }
    $manifest = $argv[1];
    $metadata = @lstat($manifest);
    if (!$metadata || !is_file($manifest) || is_link($manifest) || $metadata['size'] > 4194304) {
        throw new RuntimeException('invalid-input');
    }
    $bytes = file_get_contents($manifest);
    if (!is_string($bytes) || strlen($bytes) > 4194304
        || preg_match('//u', $bytes) !== 1 || stripos($bytes, '<!DOCTYPE') !== false) {
        throw new RuntimeException('invalid-input');
    }
    $report['manifest_sha256'] = hash('sha256', $bytes);
    $document = new DOMDocument();
    libxml_use_internal_errors(true);
    if (!$document->loadXML($bytes, LIBXML_NONET)
        || !$document->documentElement
        || $document->documentElement->localName !== 'MPD'
        || $document->documentElement->namespaceURI !== 'urn:mpeg:dash:schema:mpd:2011') {
        throw new RuntimeException('invalid-input');
    }
    $xpath = new DOMXPath($document);
    $xpath->registerNamespace('xlink', 'http://www.w3.org/1999/xlink');
    $xpath->registerNamespace('xi', 'http://www.w3.org/2001/XInclude');
    if ($xpath->query('//@xlink:* | //xi:* | //processing-instruction("xml-stylesheet")')->length !== 0) {
        throw new RuntimeException('external-resolution-unsupported');
    }
    libxml_clear_errors();
    $report['engine_revision'] = local_revision($root);
    // The Rust parent owns this private directory and keeps it until native
    // process-tree cleanup is confirmed, including timeout and cancellation.
    $workspace = __DIR__ . '/engine-' . bin2hex(random_bytes(16));
    if (!mkdir($workspace, 0700)) {
        throw new RuntimeException('workspace-unavailable');
    }
    $identity = lstat($workspace);
    $configuration = $workspace . '/saxon.xml';
    if (file_put_contents($configuration,
        '<configuration xmlns="http://saxon.sf.net/ns/configuration" edition="HE">'
        . '<global allowedProtocols="file" allowExternalFunctions="false" xInclude="false"/>'
        . '</configuration>') === false) {
        throw new RuntimeException('workspace-unavailable');
    }
    $schema = realpath($root . '/resources/xsd/DASH5th.xsd');
    $xlink = realpath($root . '/resources/xsd/xlink.xsd');
    $stylesheet = realpath($root . '/schematron/output/val_schema.xsl');
    $saxon = realpath($root . '/schematron/saxon12he.jar');
    if (!$schema || !$xlink || !$stylesheet || !$saxon) {
        throw new RuntimeException('engine-assets-unavailable');
    }
    $styleBytes = file_get_contents($stylesheet);
    if (!is_string($styleBytes) || strlen($styleBytes) > 4194304
        || preg_match('/(?:doc(?:ument)?|collection|unparsed-text|resolve-uri|parse-xml|function-lookup|transform|load-xquery-module)\\s*\\(|xsl:(?:include|import|result-document|evaluate)|<!DOCTYPE/i', $styleBytes) !== 0) {
        throw new RuntimeException('unsupported-stylesheet-dependencies');
    }
    // Resolve only the official schema and its bundled XLink import. Source
    // schemaLocation attributes never select a schema or authorize a fetch.
    libxml_set_external_entity_loader(static function ($public, $system, $context) use ($schema, $xlink) {
        if (in_array($system, ['http://www.w3.org/XML/2008/06/xlink.xsd',
            'https://www.w3.org/XML/2008/06/xlink.xsd', $xlink], true)) {
            return fopen($xlink, 'rb');
        }
        if ($system === $schema || $system === 'file://' . $schema) {
            return fopen($schema, 'rb');
        }
        return null;
    });
    libxml_clear_errors();
    $xsdPassed = $document->schemaValidate($schema);
    $xsdErrors = libxml_get_errors();
    $report['checks']['xsd'] = [
        'completed' => true, 'passed' => $xsdPassed,
        'issue_count' => $xsdPassed ? 0 : max(1, count($xsdErrors)),
    ];
    $svrl = $workspace . '/schematron.xml';
    // Fixed argv bypasses the shell. Saxon cannot load network resources,
    // expand DTDs/XInclude, use instance-selected schemas, or call Java methods.
    $process = proc_open([
        'java', '-Djavax.xml.accessExternalDTD=',
        '-Djavax.xml.accessExternalSchema=file',
        '-Djavax.xml.accessExternalStylesheet=file',
        '-jar', $saxon, '-config:' . $configuration,
        '-versionmsg:off', '-dtd:off', '-expand:off', '-xi:off', '-ext:off',
        '-s:' . $manifest, '-o:' . $svrl, '-xsl:' . $stylesheet,
    ], [0 => ['pipe', 'r'], 1 => STDERR, 2 => STDERR], $pipes);
    if (!is_resource($process)) {
        throw new RuntimeException('engine-unavailable');
    }
    fclose($pipes[0]);
    if (proc_close($process) !== 0 || !is_file($svrl) || filesize($svrl) > 16777216) {
        throw new RuntimeException('incomplete-schematron-report');
    }
    $resultDocument = new DOMDocument();
    // SVRL is a newly generated private file, not an external XML dependency.
    $svrlBytes = file_get_contents($svrl);
    if (!is_string($svrlBytes) || stripos($svrlBytes, '<!DOCTYPE') !== false
        || !$resultDocument->loadXML($svrlBytes, LIBXML_NONET)
        || !$resultDocument->documentElement
        || $resultDocument->documentElement->namespaceURI !== 'http://purl.oclc.org/dsdl/svrl'
        || $resultDocument->documentElement->localName !== 'schematron-output') {
        throw new RuntimeException('invalid-schematron-report');
    }
    $issues = $resultDocument->getElementsByTagNameNS('http://purl.oclc.org/dsdl/svrl', 'failed-assert')->length;
    $report['checks']['schematron'] = ['completed' => true, 'passed' => $issues === 0, 'issue_count' => $issues];
    $report['passed'] = $report['checks']['xsd']['passed'] && $report['checks']['schematron']['passed'];
    $exit = $report['passed'] ? 0 : 1;
} catch (Throwable $error) {
    // Fixed result fields only. Exception strings can carry paths, XML, or signaling.
    $report['passed'] = false;
    $exit = 2;
} finally {
    if ($workspace !== null && $identity !== null) {
        try {
            remove_owned_workspace($workspace, $identity);
        } catch (Throwable $cleanupError) {
            $report['passed'] = false;
            $exit = 2;
        }
    }
}
echo json_encode($report, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR), "\n";
exit($exit);
