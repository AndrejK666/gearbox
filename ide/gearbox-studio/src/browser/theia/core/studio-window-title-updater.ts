// Keep the window anchored to the open product, including while editing its sources.
import { FrontendApplication, Saveable, Widget } from "@theia/core/lib/browser";
import { WindowTitleUpdater } from "@theia/core/lib/browser/window/window-title-updater";
import { inject, injectable } from "@theia/core/shared/inversify";
import { ProductStore } from "../../product-store";
import { GearSessionService } from "../../shell/gear-session-service";

@injectable()
export class StudioWindowTitleUpdater extends WindowTitleUpdater {
  @inject(ProductStore) protected readonly products!: ProductStore;
  @inject(GearSessionService) protected readonly gears!: GearSessionService;

  override onStart(app: FrontendApplication): void {
    super.onStart(app);
    // Opening/renaming a product can leave the same widget active.
    const updateSubject = (): void => this.windowTitleService.update({ activeEditorShort: this.subject() });
    this.products.onChanged(updateSubject);
    this.gears.onDidChange(updateSubject);
  }

  protected override updateTitleWidget(widget?: Widget): void {
    this.windowTitleService.update({
      activeEditorShort: this.subject(),
      dirty: Saveable.isDirty(widget) ? "●" : "",
    });
  }

  protected subject(): string | undefined {
    const gear = this.gears.current;
    if (gear) return gear.label;
    const { open, intent } = this.products.current;
    return open ? intent?.display_name ?? open.label : undefined;
  }
}
