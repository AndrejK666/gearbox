import { codicon, ReactWidget } from "@theia/core/lib/browser";
import { CommandRegistry } from "@theia/core/lib/common";
import { inject, injectable, postConstruct } from "@theia/core/shared/inversify";
import React from "@theia/core/shared/react";
import { rowKey } from "../../common/protocol";
import { CatalogueStore } from "../catalogue-store";
import { ProductEditService } from "../product-edit-service";
import { ProductStore } from "../product-store";
import { RevealService } from "../reveal-service";
import { ADD_GEAR, SHOW_CATALOGUE_DIAGNOSTICS, SHOW_PRODUCT } from "../shell/session-command-ids";
import { CatalogueTree } from "./catalogue-tree";
import { CatalogueGrouping } from "./catalogue-grouping";
import type { CatalogueNode } from "./catalogue-model";
@injectable()
export class CatalogueWidget extends ReactWidget {
    static readonly ID = "gearbox.catalogue";
    static readonly LABEL = "Gearbox Catalogue";
    @inject(CatalogueStore)
    protected readonly store!: CatalogueStore;
    @inject(RevealService)
    protected readonly reveals!: RevealService;
    @inject(ProductEditService)
    protected readonly edits!: ProductEditService;
    @inject(ProductStore)
    protected readonly product!: ProductStore;
    @inject(CommandRegistry)
    protected readonly commands!: CommandRegistry;
    protected filter = "";
    @postConstruct()
    protected init(): void {
        this.id = CatalogueWidget.ID;
        this.title.label = CatalogueWidget.LABEL;
        this.title.caption = CatalogueWidget.LABEL;
        this.title.iconClass = codicon("library");
        this.title.closable = true;
        this.addClass("gbx-widget-catalogue");
        this.node.tabIndex = -1;
        this.toDispose.push(this.store.onChanged(() => this.update()));
        this.toDispose.push(this.product.onChanged(() => this.update()));
    }
    protected render(): React.ReactNode {
        const state = this.store.current;
        const unprojected = state.status === "ready" ? state.rows.filter(r => r.kind === "pending").length : 0;
        return <div className="gbx-root">
      <div className="gbx-header">
        {state.status === "loading" && <span className="gbx-progress">projecting {state.completed}/{state.total}</span>}
        {unprojected > 0 && <span className="gbx-error">{unprojected} did not project</span>}
        {state.diagnostics.length > 0 && <button type="button" className="gbx-catalogue-diagnostics-link" data-catalogue-diagnostics={state.diagnostics.length} onClick={() => void this.commands.executeCommand(SHOW_CATALOGUE_DIAGNOSTICS.id)}>{state.diagnostics.length} diagnostic(s) →</button>}
      </div>
      <input className="gbx-filter" type="search" placeholder="Search gears and plugins" aria-label="Filter gears" value={this.filter} onChange={e => { this.filter = e.target.value; this.update(); }}/>
      <CatalogueGrouping value={this.store.groupPlugins} onChange={value => this.store.setGroupPlugins(value)}/>
      {state.status === "error" && <div className="gbx-error" role="alert">The catalogue could not be loaded: {state.error}</div>}
      {state.failedRoots.map(r => <div className="gbx-error" role="alert" key={r.path}>Source root <code>{r.path}</code> could not be opened: {r.error}</div>)}
      <CatalogueTree surface="catalogue" rows={state.rows} grouped={this.store.groupPlugins} search={this.filter} loading={state.status === "loading"} selectedKey={this.store.selected} inProduct={id => this.edits.inProduct(id)} onSelect={node => { if (node.row)
            this.store.select(rowKey(node.row)); }} onReveal={row => void this.reveals.reveal(row.gear.source, row.gear.gdl_path)} actions={(node, selected) => this.renderAction(node, selected)}/>
    </div>;
    }
    protected renderAction(node: CatalogueNode, selected: boolean): React.ReactNode {
        const row = node.row;
        if (!row || row.kind !== "projected" || !this.edits.editable)
            return undefined;
        const id = row.gear.id, inside = this.edits.inProduct(id);
        const product = this.product.current.open?.label ?? "the product";
        const label = inside ? `Show ${row.gear.display_name} in ${product}` : node.host ? `Attach ${row.gear.display_name} to ${node.host.display_name}` : `Add ${row.gear.display_name} to ${product}`;
        return <span className="gbx-row-action"><button type="button" className="gbx-in-product" data-in-product={String(inside)} data-toggle-gear={id} aria-label={label} title={label} tabIndex={selected ? 0 : -1} onClick={e => {
                e.stopPropagation();
                if (inside) {
                    this.product.setFocus({ kind: "gear", id });
                    void this.commands.executeCommand(SHOW_PRODUCT.id, "composition");
                }
                else
                    void this.commands.executeCommand(ADD_GEAR.id, { gearId: id, host: node.host?.id, point: node.point, origin: node.key });
            }}>{inside ? "Show" : "Add"}</button></span>;
    }
}
