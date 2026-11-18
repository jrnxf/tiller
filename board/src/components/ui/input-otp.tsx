import { OTPInput, OTPInputContext } from "input-otp";
import type { ComponentProps } from "react";
import { useContext } from "react";

// A one-time code as a row of boxes, one per character: the shadcn input-otp
// parts on the `input-otp` package, with the board's own classes. One real
// input under the boxes takes typing, paste and the phone's code autofill.
export function InputOTP({
  className,
  containerClassName,
  ...props
}: ComponentProps<typeof OTPInput>) {
  return (
    <OTPInput
      containerClassName={`otp${containerClassName ? ` ${containerClassName}` : ""}`}
      className={`otp-input${className ? ` ${className}` : ""}`}
      {...props}
    />
  );
}

export function InputOTPGroup({ className, ...props }: ComponentProps<"div">) {
  return <div className={`otp-group${className ? ` ${className}` : ""}`} {...props} />;
}

export function InputOTPSlot({ index }: { index: number }) {
  const slot = useContext(OTPInputContext).slots[index];
  return (
    <div className="otp-slot" data-active={slot?.isActive ? true : undefined}>
      {slot?.char ?? <span className="otp-ph">{slot?.placeholderChar}</span>}
      {slot?.hasFakeCaret ? <span className="otp-caret" aria-hidden="true" /> : null}
    </div>
  );
}

export function InputOTPSeparator() {
  return <div className="otp-sep" role="separator" />;
}
