# License source supplements

Release packaging normally copies license and notice files directly from the
exact Cargo registry packages used for each target, along with their version,
SPDX expression, authors and source repository in `licenses/dependencies.json`.
The inventory also includes build and test dependencies resolved for that target;
it is not a claim that every listed crate is linked into the executable.

The published `objc2`, `objc2-encode`, `block2` and `dispatch2` crate archives omit
their workspace license notice. `objc2-UPSTREAM.md` retains it verbatim from
these release commits, verified to contain identical notice text:

- `objc2 0.6.4` and `dispatch2 0.3.1`:
  https://github.com/madsmtm/objc2/blob/8852b424193ca41602281b3d7540d7c8ed51e49a/LICENSE.md
- `block2 0.6.2`:
  https://github.com/madsmtm/objc2/blob/b4167b582b2f75f9a1be75495c41b765344fd03c/LICENSE.md
- `objc2-encode 4.1.0`:
  https://github.com/madsmtm/objc2/blob/8d214f5477365ffcbcbb7de058c86ed9a518efb7/LICENSE.md

That notice references the standard MIT license. `MIT-template.txt` retains the
canonical license text from the SPDX license list at
https://github.com/spdx/license-list-data/blob/31ba1a50e5397e00a304dbadc76531740e89ee48/text/MIT.txt.
Its copyright placeholders are part of that reference text; package authors are
recorded separately without inventing copyright statements.

These files describe third-party dependencies and do not license Neordr itself.
The collector fails when a dependency with missing license files changes, so its
license sources must be reviewed before producing another release. No network
requests are made while collecting or packaging licenses.
