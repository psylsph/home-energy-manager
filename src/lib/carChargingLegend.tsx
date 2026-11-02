/** Car Charging bars on the Forecast page's home-use chart (issue #406). */

/** The series name, shared by the bars and their legend label. */
export const CAR_CHARGING_NAME = 'Car Charging';
/** The EV purple in the median lines' lighter shade. */
export const CAR_CHARGING_COLOR = '#c084fc';
/** The bars' fill. The legend icon takes it too, so it is drawn as
 *  translucent as the bars rather than as a solid block. */
export const CAR_CHARGING_FILL = 'rgba(192, 132, 252, 0.6)';

/** Legend formatter: keeps the Car Charging label as solid as the others,
 *  as its translucent fill would otherwise dim the text too. */
export function solidLegendLabel(value: string) {
  return value === CAR_CHARGING_NAME ? (
    <span style={{ color: CAR_CHARGING_COLOR }}>{value}</span>
  ) : (
    value
  );
}
