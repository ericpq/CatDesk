# Extension boundary

CatDesk core is intentionally domain-agnostic. It provides local coding-agent capabilities such as
filesystem access, shell execution, Git operations, code navigation, checkpoints, and the CatDesk UI.

Business-specific integrations should not be implemented directly in CatDesk core.

Examples of code that belongs outside this repository:

- vendor-specific API base URLs and endpoint paths;
- organization-specific authentication or session formats;
- industry-specific report names, entity identifiers, filters, or terminology;
- private operational workflows and data-query logic.

## Recommended architecture

Run private or domain-specific capabilities as a separate MCP server or connector and connect it
alongside CatDesk:

```text
ChatGPT
├── CatDesk                 # general-purpose local coding agent
└── Private connector       # organization/domain-specific tools
    └── external service
```

This separation keeps CatDesk reusable and auditable while allowing a deployment to add private
capabilities without publishing them in the CatDesk source tree.

## Security expectations

Private connectors should keep credentials outside source control, expose only the minimum required
tools, validate inputs, bound response sizes, and avoid returning secrets in tool output or logs.

A contribution to CatDesk core should remain useful without knowledge of a particular company,
vendor, industry, or private deployment.
